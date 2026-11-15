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

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useMutation } from '@tanstack/react-query';
import type { ViewerInstance } from 'jit-viewer';
import {
  Download, ChevronLeft, ChevronRight, X,
} from 'lucide-react';

import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogTitle } from '@/components/ui/dialog';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { Skeleton } from '@/components/ui/skeleton';
import { useTheme } from '@/context/theme-context';
import { toast } from '@/hooks/use-toast';
import { useTranslation } from 'react-i18next';

import { preview_attachment, download_attachment } from '@/api/mailbox/envelope/api';
import { getFileConfig } from './mail-message-view';

const PREVIEWABLE_IMAGE = /^image\/(png|jpeg|gif|webp|svg\+xml)$/;

const EXT_TO_MIME: Record<string, string> = {
  '.pdf': 'application/pdf',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.jpeg': 'image/jpeg',
  '.gif': 'image/gif',
  '.webp': 'image/webp',
  '.svg': 'image/svg+xml',
  '.txt': 'text/plain',
  '.csv': 'text/csv',
  '.html': 'text/html',
  '.htm': 'text/html',
  '.xml': 'application/xml',
  '.css': 'text/css',
  '.js': 'application/javascript',
  '.mjs': 'application/javascript',
  '.md': 'text/markdown',
  '.json': 'application/json',
  '.php': 'application/x-httpd-php',
  '.sh': 'application/x-sh',
  '.pl': 'application/x-perl',
  '.py': 'application/x-python',
  '.rb': 'application/x-ruby',
};

function getExtensionMime(filename: string): string | null {
  const dot = filename.lastIndexOf('.');
  if (dot < 0) return null;
  const ext = filename.slice(dot).toLowerCase();
  return EXT_TO_MIME[ext] ?? null;
}

function resolveContentType(contentType: string, fileName: string): string {
  if (contentType && contentType !== 'application/octet-stream') return contentType;
  return getExtensionMime(fileName) ?? contentType;
}

export interface PreviewAttachment {
  content_hash: string;
  file_type: string;
  filename: string;
}

interface AttachmentPreviewProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  accountId: number;
  envelopeId: string;
  contentHash: string;
  contentType: string;
  fileName: string;
  /** Full attachment list for gallery navigation (optional). */
  attachments?: PreviewAttachment[];
  /** Index of the current attachment within `attachments`. */
  attachmentIndex?: number;
}

function isImagePreview(contentType: string, fileName?: string) {
  return PREVIEWABLE_IMAGE.test(resolveContentType(contentType, fileName ?? ''));
}

// jit-viewer renders HTML attachments in an iframe that allows scripts and
// same-origin access, which would let a malicious attachment read the app's
// origin. HTML is rendered by a fully sandboxed iframe instead.
function isHtmlPreview(contentType: string, fileName?: string) {
  const type = resolveContentType(contentType, fileName ?? '').toLowerCase();
  return type === 'text/html' || type === 'application/xhtml+xml';
}

const LEGACY_WORD_MIME = /^application\/(msword|vnd\.ms-word|x-msword)$/;

// jit-viewer only handles the OOXML (.docx) format; Word 97-2003 binary
// documents are parsed locally by @file-viewer/doc.
function isLegacyDocPreview(contentType: string, fileName?: string) {
  const type = resolveContentType(contentType, fileName ?? '').toLowerCase();
  return LEGACY_WORD_MIME.test(type) || /\.(doc|dot)$/i.test(fileName ?? '');
}

export default function AttachmentPreview({
  open,
  onOpenChange,
  accountId,
  envelopeId,
  contentHash,
  contentType,
  fileName,
  attachments,
  attachmentIndex,
}: AttachmentPreviewProps) {
  const { t, i18n } = useTranslation();
  const { resolvedTheme } = useTheme();
  const [blob, setBlob] = useState<Blob | null>(null);
  const [htmlUrl, setHtmlUrl] = useState<string | null>(null);
  const [legacyDocHtml, setLegacyDocHtml] = useState<string | null>(null);
  const [legacyDocLoading, setLegacyDocLoading] = useState(false);
  const containerRef = useRef<HTMLDivElement | null>(null);

  // ── Gallery state ──────────────────────────────────────────────
  // When attachments list is provided, compute image-only indices for navigation.
  const imageIndices = useMemo(() => {
    if (!attachments) return [];
    return attachments
      .map((a, i) => (isImagePreview(a.file_type, a.filename) ? i : -1))
      .filter((i) => i >= 0);
  }, [attachments]);

  const [currentIndex, setCurrentIndex] = useState(attachmentIndex ?? 0);

  // Reset to the clicked attachment every time the dialog opens.
  useEffect(() => {
    if (open) {
      setCurrentIndex(attachmentIndex ?? 0);
    }
  }, [open, attachmentIndex]);

  // Resolve which attachment to display.
  const resolved = useMemo(() => {
    if (attachments && currentIndex < attachments.length) {
      const a = attachments[currentIndex];
      return {
        contentHash: a.content_hash,
        contentType: a.file_type,
        fileName: a.filename,
      };
    }
    return { contentHash, contentType, fileName };
  }, [attachments, currentIndex, contentHash, contentType, fileName]);

  // Position within image-only list (for "3 / 12" counter).
  const imagePos = imageIndices.indexOf(currentIndex); // -1 if not an image
  const imageTotal = imageIndices.length;

  const goPrev = useCallback(() => {
    if (imagePos > 0) setCurrentIndex(imageIndices[imagePos - 1]);
  }, [imagePos, imageIndices]);

  const goNext = useCallback(() => {
    if (imagePos < imageTotal - 1) setCurrentIndex(imageIndices[imagePos + 1]);
  }, [imagePos, imageTotal, imageIndices]);

  // Keyboard navigation
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'ArrowLeft') goPrev();
      else if (e.key === 'ArrowRight') goNext();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, goPrev, goNext]);

  const effectiveType = resolveContentType(resolved.contentType, resolved.fileName);
  const isHtml = isHtmlPreview(resolved.contentType, resolved.fileName);
  const isLegacyDoc = isLegacyDocPreview(resolved.contentType, resolved.fileName);

  // ── Fetch preview blob ─────────────────────────────────────────
  const previewMutation = useMutation({
    mutationFn: () => preview_attachment(accountId, envelopeId, resolved.contentHash),
    onSuccess: (data) => setBlob(data),
    onError: (error: Error) => {
      toast({
        title: t('attachment_preview.failedToLoad'),
        description: error.message,
        variant: 'destructive',
      });
    },
  });

  const { mutate: fetchPreview } = previewMutation;

  useEffect(() => {
    if (open) {
      setBlob(null);
      fetchPreview();
    }
  }, [open, resolved.contentHash, fetchPreview]);

  // HTML attachments run in an iframe with no scripts and no same-origin access.
  useEffect(() => {
    if (!open || !blob || !isHtml) {
      setHtmlUrl(null);
      return;
    }
    const url = URL.createObjectURL(new Blob([blob], { type: 'text/html' }));
    setHtmlUrl(url);
    return () => URL.revokeObjectURL(url);
  }, [open, blob, isHtml]);

  // Legacy Word (.doc/.dot) is parsed to sanitized HTML in the browser, then
  // displayed in the same locked-down iframe as HTML attachments.
  useEffect(() => {
    if (!open || !blob || !isLegacyDoc) {
      setLegacyDocHtml(null);
      setLegacyDocLoading(false);
      return;
    }

    let cancelled = false;
    setLegacyDocLoading(true);

    import('@file-viewer/doc')
      .then(({ parseMsDocToHtml }) =>
        parseMsDocToHtml(blob, {
          renderOptions: {
            externalLinkPolicy: 'block',
            externalResourcePolicy: 'block',
          },
        }),
      )
      .then((rendered) => {
        if (cancelled) return;
        setLegacyDocHtml(
          `<!doctype html><html><head><meta charset="utf-8">` +
            `<style>${rendered.css}</style></head>` +
            `<body><div class="msdoc-root">${rendered.html}</div></body></html>`,
        );
      })
      .catch((error: Error) => {
        if (cancelled) return;
        toast({
          title: t('attachment_preview.failedToLoad'),
          description: error.message,
          variant: 'destructive',
        });
      })
      .finally(() => {
        if (!cancelled) setLegacyDocLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [open, blob, isLegacyDoc, t]);

  // ── Render with jit-viewer ─────────────────────────────────────
  // jit-viewer detects the format from the file name and MIME type, and
  // handles PDF, Office, CSV, text/code, images, audio, video, CAD and 3D.
  useEffect(() => {
    if (!open || !blob || isHtml || isLegacyDoc) return;
    const container = containerRef.current;
    if (!container) return;

    const file = new File(
      [blob],
      resolved.fileName || 'attachment',
      { type: effectiveType || blob.type || 'application/octet-stream' },
    );

    const isLightTheme = resolvedTheme === 'light' || resolvedTheme.endsWith('-light');
    let cancelled = false;
    let viewer: ViewerInstance | undefined;

    // Load the (large) viewer SDK only when a preview is actually opened.
    Promise.all([import('jit-viewer'), import('jit-viewer/style.css')])
      .then(([{ createViewer }]) => {
        if (cancelled) return;
        viewer = createViewer({
          target: container,
          file,
          filename: resolved.fileName,
          toolbar: {
            position: 'bottom',
            items: [
              { type: 'zoom' },
              { type: 'rotate' },
              { type: 'fullscreen' },
              { type: 'pagination' },
              { type: 'video' },
              { type: 'model3d' },
            ],
          },
          theme: isLightTheme ? 'light' : 'dark',
          locale: i18n.language?.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en',
          width: '100%',
          height: '100%',
          onError: (error) => {
            toast({
              title: t('attachment_preview.failedToLoad'),
              description: error.message,
              variant: 'destructive',
            });
          },
        });
        viewer.mount().catch((error: Error) => {
          if (cancelled) return;
          toast({
            title: t('attachment_preview.failedToLoad'),
            description: error.message,
            variant: 'destructive',
          });
        });
      })
      .catch((error: Error) => {
        if (cancelled) return;
        toast({
          title: t('attachment_preview.failedToLoad'),
          description: error.message,
          variant: 'destructive',
        });
      });

    return () => {
      cancelled = true;
      viewer?.destroy();
    };
  }, [open, blob, isHtml, isLegacyDoc, effectiveType, resolved.fileName, resolvedTheme, i18n.language, t]);

  const handleDownload = () => {
    download_attachment(accountId, envelopeId, resolved.contentHash, resolved.fileName);
  };

  const { icon } = useMemo(() => getFileConfig(resolved.contentType), [resolved.contentType]);

  const isImage = isImagePreview(resolved.contentType, resolved.fileName);
  const showArrows = imageTotal > 1 && isImage;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent aria-describedby={undefined}
        className="w-screen h-screen max-w-none rounded-none p-0 gap-0 border-0 bg-slate-700/10 flex flex-col overflow-hidden"
        hideClose
        hideFullscreen
      >
        <DialogTitle className='sr-only'>{resolved.fileName}</DialogTitle>
        {/* Header */}
        <div className="z-20 flex shrink-0 items-center justify-between gap-3 bg-slate-900/95 px-4 py-2 text-white">
          <div className="flex min-w-0 items-center gap-2">
            {icon}
            <span className="text-sm font-medium truncate max-w-[400px]">
              {resolved.fileName}
            </span>
            {imagePos >= 0 && imageTotal > 1 && (
              <span className="text-xs text-white/60 ml-1">
                {imagePos + 1} / {imageTotal}
              </span>
            )}
          </div>
          <div className="flex items-center gap-1">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button
                  variant="ghost"
                  size="icon"
                  className="h-8 w-8 text-white hover:bg-white/20"
                  onClick={handleDownload}
                >
                  <Download className="h-4 w-4" />
                </Button>
              </TooltipTrigger>
              <TooltipContent>{t('attachment.download')}</TooltipContent>
            </Tooltip>
            <Button
              variant="ghost"
              size="icon"
              className="h-8 w-8 text-white hover:bg-white/20"
              onClick={() => onOpenChange(false)}
            >
              <X className="h-4 w-4" />
            </Button>
          </div>
        </div>

        {/* Preview body */}
        <div className="relative min-h-0 flex-1">
          {isHtml ? (
            htmlUrl ? (
              <iframe
                src={htmlUrl}
                sandbox=""
                title={resolved.fileName}
                className="w-full h-full border-0 bg-white"
              />
            ) : null
          ) : isLegacyDoc ? (
            legacyDocHtml ? (
              <iframe
                srcDoc={legacyDocHtml}
                sandbox=""
                title={resolved.fileName}
                className="w-full h-full border-0 bg-white"
              />
            ) : null
          ) : (
            <div ref={containerRef} className="w-full h-full" />
          )}

          {(previewMutation.isPending || legacyDocLoading) && (
            <div className="absolute inset-0 flex items-center justify-center">
              <div className="flex flex-col items-center gap-3">
                <Skeleton className="w-64 h-4 bg-white/10" />
                <Skeleton className="w-48 h-4 bg-white/10" />
                <Skeleton className="w-56 h-4 bg-white/10" />
              </div>
            </div>
          )}

          {/* Navigation arrows (images only) */}
          {showArrows && (
            <>
              <Button
                variant="ghost"
                size="icon"
                disabled={imagePos <= 0}
                className="absolute left-2 top-1/2 -translate-y-1/2 z-10 h-10 w-10 rounded-full text-white bg-black/40 hover:bg-black/60 disabled:opacity-30 disabled:hover:bg-transparent"
                onClick={goPrev}
              >
                <ChevronLeft className="h-6 w-6" />
              </Button>
              <Button
                variant="ghost"
                size="icon"
                disabled={imagePos >= imageTotal - 1}
                className="absolute right-2 top-1/2 -translate-y-1/2 z-10 h-10 w-10 rounded-full text-white bg-black/40 hover:bg-black/60 disabled:opacity-30 disabled:hover:bg-transparent"
                onClick={goNext}
              >
                <ChevronRight className="h-6 w-6" />
              </Button>
            </>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
