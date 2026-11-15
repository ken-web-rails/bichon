//
// Copyright (c) 2025-2026 rustmailer.com (https://rustmailer.com)
//
// This file is part of the Bichon Email Archiving Project
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

//! `bichon-admin stats`: count archived emails per year, per month and per
//! sender.
//!
//! Email metadata is **not** stored in memdb — memdb only holds accounts,
//! users, roles and configuration. The sender (`from`) and message date
//! (`date`) of every archived message live in the Tantivy envelope index under
//! `<index-root>/mail_metadata`. This command opens that index read-only,
//! iterates every live document and aggregates those two stored fields. It
//! never writes to the archive, so it can run while the server is up.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use bichon_core::{
    error::{code::ErrorCode, BichonResult},
    raise_error,
    settings::dir::resolve_dirs,
    store::tantivy::fields::{F_DATE, F_FROM},
};
use chrono::{DateTime, Datelike, Utc};
use clap::{Parser, ValueEnum};
use console::style;
use serde::Serialize;
use tantivy::{
    schema::{Field, Value},
    Index, Searcher, TantivyDocument,
};

/// Sender shown for documents whose `from` field is empty.
const UNKNOWN_SENDER: &str = "(unknown sender)";

#[derive(Parser, Debug, Default)]
pub struct StatsArgs {
    /// bichon data root (same value as the server's `--bichon-root-dir`).
    #[clap(
        long,
        visible_aliases = ["root-dir", "bichon-root-dir"],
        env = "BICHON_ROOT_DIR",
        help = "Path of the bichon data root (same value as the server's --bichon-root-dir)"
    )]
    pub root: Option<String>,

    /// Index parent directory (default: `<root>/bichon-indices`).
    #[clap(
        long = "index-dir",
        visible_aliases = ["index-parent", "bichon-index-dir"],
        env = "BICHON_INDEX_DIR",
        help = "Parent directory of bichon-indices (default: <root>); same value as the server's --bichon-index-dir"
    )]
    pub index_parent: Option<String>,

    /// Output format.
    #[clap(long, value_enum, default_value = "text", help = "Output format")]
    pub format: StatsFormat,

    /// Number of senders to list, highest first (`0` = all).
    #[clap(long, default_value_t = 20, help = "Top N senders by count (0 = all)")]
    pub top: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum StatsFormat {
    #[default]
    Text,
    Json,
}

pub fn run_cli(args: StatsArgs) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("\n{} {e:?}", style("ERROR:").red().bold());
            1
        }
    }
}

fn run(args: StatsArgs) -> BichonResult<()> {
    let root = resolve_root(args.root.as_deref())?;
    let dirs = resolve_dirs(&root, args.index_parent.as_deref().map(Path::new), None);
    let index = open_index(&dirs.envelope_dir)?;
    let stats = scan(&index)?;

    match args.format {
        StatsFormat::Text => print_text(&stats, args.top),
        StatsFormat::Json => print_json(&stats, args.top)?,
    }
    Ok(())
}

/// Resolve the data root from the flag/env value. The stats command reads an
/// existing archive, so a missing root is an error rather than a prompt.
fn resolve_root(root: Option<&str>) -> BichonResult<PathBuf> {
    let Some(root) = root.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(raise_error!(
            "missing 'bichon_root_dir': pass --root or set BICHON_ROOT_DIR".into(),
            ErrorCode::InvalidParameter
        ));
    };
    let path = PathBuf::from(root);
    if !path.is_absolute() {
        return Err(raise_error!(
            "bichon_root_dir must be an absolute directory path".into(),
            ErrorCode::InvalidParameter
        ));
    }
    if !path.exists() {
        return Err(raise_error!(
            format!("bichon_root_dir does not exist: {}", path.display()),
            ErrorCode::InvalidParameter
        ));
    }
    Ok(path)
}

struct EnvelopeIndex {
    searcher: Searcher,
    from_field: Field,
    date_field: Field,
}

/// Open the envelope index read-only and resolve the two fields we need.
fn open_index(dir: &Path) -> BichonResult<EnvelopeIndex> {
    if !dir.exists() {
        return Err(raise_error!(
            format!("envelope index not found: {}", dir.display()),
            ErrorCode::InvalidParameter
        ));
    }
    let index = Index::open_in_dir(dir).map_err(|e| {
        raise_error!(
            format!("cannot open envelope index {}: {e:#?}", dir.display()),
            ErrorCode::InternalError
        )
    })?;

    let from_field = index.schema().get_field(F_FROM).map_err(|e| {
        raise_error!(
            format!("envelope index has no '{F_FROM}' field: {e:#?}"),
            ErrorCode::InternalError
        )
    })?;
    let date_field = index.schema().get_field(F_DATE).map_err(|e| {
        raise_error!(
            format!("envelope index has no '{F_DATE}' field: {e:#?}"),
            ErrorCode::InternalError
        )
    })?;

    let reader = index.reader().map_err(|e| {
        raise_error!(
            format!("cannot create reader for {}: {e:#?}", dir.display()),
            ErrorCode::InternalError
        )
    })?;
    reader.reload().map_err(|e| {
        raise_error!(
            format!("cannot reload reader for {}: {e:#?}", dir.display()),
            ErrorCode::InternalError
        )
    })?;

    Ok(EnvelopeIndex {
        searcher: reader.searcher(),
        from_field,
        date_field,
    })
}

#[derive(Default)]
struct Stats {
    total: u64,
    /// Documents whose `date` is missing or not positive.
    unknown_date: u64,
    /// Documents whose stored fields could not be read.
    unreadable: u64,
    by_year: BTreeMap<i32, u64>,
    by_month: BTreeMap<(i32, u32), u64>,
    /// sender -> year (`None` when the Date header was unusable) -> count.
    by_sender_year: BTreeMap<String, BTreeMap<Option<i32>, u64>>,
}

fn scan(index: &EnvelopeIndex) -> BichonResult<Stats> {
    let mut stats = Stats::default();

    for segment in index.searcher.segment_readers() {
        let store = segment.get_store_reader(1).map_err(|e| {
            raise_error!(
                format!("cannot read document store: {e:#?}"),
                ErrorCode::InternalError
            )
        })?;
        let alive = segment.alive_bitset();

        for doc_id in 0..segment.max_doc() {
            if let Some(alive) = alive {
                if !alive.is_alive(doc_id) {
                    continue;
                }
            }

            let doc: TantivyDocument = match store.get(doc_id) {
                Ok(doc) => doc,
                Err(_) => {
                    stats.unreadable += 1;
                    continue;
                }
            };

            let sender = doc
                .get_first(index.from_field)
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let date_ms = doc
                .get_first(index.date_field)
                .and_then(|value| value.as_i64())
                .unwrap_or(0);

            record(&mut stats, sender, date_ms);
        }
    }

    Ok(stats)
}

/// Add one message to the aggregates. `date_ms` is epoch milliseconds from the
/// RFC 5322 `Date` header; `0` means the header was absent.
fn record(stats: &mut Stats, sender: &str, date_ms: i64) {
    stats.total += 1;

    let sender = sender.trim();
    let sender = if sender.is_empty() {
        UNKNOWN_SENDER
    } else {
        sender
    };

    let year = match DateTime::<Utc>::from_timestamp_millis(date_ms) {
        Some(date) if date_ms > 0 => {
            *stats.by_year.entry(date.year()).or_insert(0) += 1;
            *stats
                .by_month
                .entry((date.year(), date.month()))
                .or_insert(0) += 1;
            Some(date.year())
        }
        _ => {
            stats.unknown_date += 1;
            None
        }
    };

    *stats
        .by_sender_year
        .entry(sender.to_string())
        .or_default()
        .entry(year)
        .or_insert(0) += 1;
}

struct SenderGroup<'a> {
    sender: &'a str,
    years: &'a BTreeMap<Option<i32>, u64>,
    total: u64,
}

/// Sender groups ordered by their total count (descending), ties broken by
/// address. `top` limits the number of *senders*, not the year rows.
fn top_sender_groups<'a>(
    by_sender_year: &'a BTreeMap<String, BTreeMap<Option<i32>, u64>>,
    top: usize,
) -> Vec<SenderGroup<'a>> {
    let mut groups: Vec<SenderGroup<'a>> = by_sender_year
        .iter()
        .map(|(sender, years)| SenderGroup {
            sender: sender.as_str(),
            years,
            total: years.values().copied().sum(),
        })
        .collect();
    groups.sort_by(|a, b| b.total.cmp(&a.total).then_with(|| a.sender.cmp(b.sender)));
    if top > 0 {
        groups.truncate(top);
    }
    groups
}

/// Flatten sender groups into `(sender, year, count)` rows, years ascending.
fn sender_year_rows(groups: &[SenderGroup<'_>]) -> Vec<(String, String, u64)> {
    let mut rows = Vec::new();
    for group in groups {
        for (year, count) in group.years {
            let year = (*year)
                .map(|year| format!("{year:04}"))
                .unwrap_or_else(|| "unknown".to_string());
            rows.push((group.sender.to_string(), year, *count));
        }
    }
    rows
}

fn print_text(stats: &Stats, top: usize) {
    let mut summary = vec![vec!["Archived emails".to_string(), stats.total.to_string()]];
    if stats.unknown_date > 0 {
        summary.push(vec![
            "Without a usable Date header".to_string(),
            stats.unknown_date.to_string(),
        ]);
    }
    if stats.unreadable > 0 {
        summary.push(vec![
            "Unreadable documents skipped".to_string(),
            stats.unreadable.to_string(),
        ]);
    }
    print_table(&["Metric", "Value"], &summary);

    println!("\nBy year");
    let year_rows: Vec<Vec<String>> = stats
        .by_year
        .iter()
        .map(|(year, count)| vec![format!("{year:04}"), count.to_string()])
        .collect();
    print_table(&["Year", "Emails"], &year_rows);

    println!("\nBy month");
    let month_rows: Vec<Vec<String>> = stats
        .by_month
        .iter()
        .map(|((year, month), count)| vec![format!("{year:04}-{month:02}"), count.to_string()])
        .collect();
    print_table(&["Month", "Emails"], &month_rows);

    println!("\nBy sender and year");
    let groups = top_sender_groups(&stats.by_sender_year, top);
    let sender_rows: Vec<Vec<String>> = sender_year_rows(&groups)
        .into_iter()
        .map(|(sender, year, count)| vec![sender, year, count.to_string()])
        .collect();
    print_table(&["Sender", "Year", "Emails"], &sender_rows);

    if groups.len() < stats.by_sender_year.len() {
        println!(
            "... {} more sender(s); use --top 0 to show all",
            stats.by_sender_year.len() - groups.len()
        );
    }
}

/// Print a simple ASCII table; the last column is right-aligned for counts.
fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut widths: Vec<usize> = headers
        .iter()
        .map(|header| header.chars().count())
        .collect();
    for row in rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index].max(cell.chars().count());
        }
    }

    let rule = table_rule(&widths);
    println!("{rule}");
    let header_cells: Vec<String> = headers.iter().map(|header| header.to_string()).collect();
    print_table_row(&header_cells, &widths, false);
    println!("{rule}");
    if !rows.is_empty() {
        for row in rows {
            print_table_row(row, &widths, true);
        }
        println!("{rule}");
    }
}

fn table_rule(widths: &[usize]) -> String {
    let mut rule = String::from("+");
    for width in widths {
        rule.push_str(&"-".repeat(width + 2));
        rule.push('+');
    }
    rule
}

fn print_table_row(cells: &[String], widths: &[usize], right_align_last: bool) {
    let last = cells.len().saturating_sub(1);
    let mut line = String::from("|");
    for (index, cell) in cells.iter().enumerate() {
        let padding = widths[index].saturating_sub(cell.chars().count());
        line.push(' ');
        if right_align_last && index == last {
            line.push_str(&" ".repeat(padding));
            line.push_str(cell);
        } else {
            line.push_str(cell);
            line.push_str(&" ".repeat(padding));
        }
        line.push_str(" |");
    }
    println!("{line}");
}

#[derive(Serialize)]
struct JsonReport {
    total: u64,
    unknown_date: u64,
    unreadable: u64,
    by_year: BTreeMap<String, u64>,
    by_month: BTreeMap<String, u64>,
    sender_total: usize,
    by_sender_year: Vec<JsonSenderYear>,
}

#[derive(Serialize)]
struct JsonSenderYear {
    sender: String,
    year: String,
    count: u64,
}

fn print_json(stats: &Stats, top: usize) -> BichonResult<()> {
    // Zero-padded string keys keep BTreeMap output in chronological order.
    let by_year = stats
        .by_year
        .iter()
        .map(|(year, count)| (format!("{year:04}"), *count))
        .collect();
    let by_month = stats
        .by_month
        .iter()
        .map(|((year, month), count)| (format!("{year:04}-{month:02}"), *count))
        .collect();

    let groups = top_sender_groups(&stats.by_sender_year, top);
    let report = JsonReport {
        total: stats.total,
        unknown_date: stats.unknown_date,
        unreadable: stats.unreadable,
        by_year,
        by_month,
        sender_total: stats.by_sender_year.len(),
        by_sender_year: sender_year_rows(&groups)
            .into_iter()
            .map(|(sender, year, count)| JsonSenderYear {
                sender,
                year,
                count,
            })
            .collect(),
    };
    let json = serde_json::to_string_pretty(&report).map_err(|e| {
        raise_error!(
            format!("cannot serialize statistics: {e:#?}"),
            ErrorCode::InternalError
        )
    })?;
    println!("{json}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn millis(year: i32, month: u32, day: u32) -> i64 {
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    #[test]
    fn buckets_by_year_month_and_sender_year() {
        let mut stats = Stats::default();
        record(&mut stats, "alice@example.com", millis(2024, 2, 10));
        record(&mut stats, "alice@example.com", millis(2025, 1, 5));
        record(&mut stats, "bob@example.com", millis(2024, 2, 20));
        record(&mut stats, "  ", 0);

        assert_eq!(stats.total, 4);
        assert_eq!(stats.unknown_date, 1);
        assert_eq!(stats.by_year.get(&2024), Some(&2));
        assert_eq!(stats.by_year.get(&2025), Some(&1));
        assert_eq!(stats.by_month.get(&(2024, 2)), Some(&2));
        assert_eq!(stats.by_month.get(&(2025, 1)), Some(&1));

        let alice = stats.by_sender_year.get("alice@example.com").unwrap();
        assert_eq!(alice.get(&Some(2024)), Some(&1));
        assert_eq!(alice.get(&Some(2025)), Some(&1));
        let bob = stats.by_sender_year.get("bob@example.com").unwrap();
        assert_eq!(bob.get(&Some(2024)), Some(&1));
        let unknown = stats.by_sender_year.get(UNKNOWN_SENDER).unwrap();
        assert_eq!(unknown.get(&None), Some(&1));
    }

    #[test]
    fn sender_groups_order_by_total_then_address() {
        let mut by_sender_year: BTreeMap<String, BTreeMap<Option<i32>, u64>> = BTreeMap::new();
        by_sender_year.insert(
            "b@example.com".to_string(),
            BTreeMap::from([(Some(2024), 2)]),
        );
        by_sender_year.insert(
            "a@example.com".to_string(),
            BTreeMap::from([(Some(2024), 2)]),
        );
        by_sender_year.insert(
            "c@example.com".to_string(),
            BTreeMap::from([(Some(2024), 3), (Some(2025), 2)]),
        );

        let all = top_sender_groups(&by_sender_year, 0);
        let summary: Vec<(&str, u64)> = all
            .iter()
            .map(|group| (group.sender, group.total))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("c@example.com", 5),
                ("a@example.com", 2),
                ("b@example.com", 2)
            ]
        );
        assert_eq!(top_sender_groups(&by_sender_year, 1).len(), 1);

        let rows = sender_year_rows(&all);
        assert_eq!(
            rows.first(),
            Some(&("c@example.com".to_string(), "2024".to_string(), 3))
        );
        assert_eq!(
            rows.last(),
            Some(&("b@example.com".to_string(), "2024".to_string(), 2))
        );
    }
}
