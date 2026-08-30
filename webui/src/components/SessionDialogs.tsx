// Rename + delete dialogs for a session (triggered from the sidebar item menu).

import { useEffect, useRef, useState } from 'preact/hooks';
import { renameSession, deleteSession, DeleteSessionError, SessionMetaWithProject } from '../api';
import { useT } from '../settings';

interface RenameDialogProps {
  session: SessionMetaWithProject;
  onClose: () => void;
  /** Called with the new name after a successful rename. */
  onDone: (name: string) => void;
}

export function RenameDialog({ session, onClose, onDone }: RenameDialogProps) {
  const t = useT();
  const [name, setName] = useState(session.name || '');
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    inputRef.current?.focus();
    inputRef.current?.select();
  }, []);

  async function submit() {
    const n = name.trim();
    if (!n || busy) return;
    setBusy(true);
    try {
      await renameSession(session.project_hash, session.id, n);
      onDone(n);
      onClose();
    } catch {
      setBusy(false);
    }
  }

  return (
    <div
      class="modal-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div class="modal-card modal-card-sm">
        <div class="modal-header">
          <span>✎</span>
          <h3>{t('rename.title')}</h3>
          <button class="ghost-btn modal-close" onClick={onClose} aria-label={t('common.cancel')}>
            ×
          </button>
        </div>
        <div class="modal-body">
          <input
            ref={inputRef}
            class="menu-input"
            type="text"
            value={name}
            placeholder={t('rename.placeholder')}
            onInput={(e) => setName((e.target as HTMLInputElement).value)}
            onKeyDown={(e) => {
              // 忽略输入法组字阶段的回车（选词确认），避免误触发保存。
              if (e.isComposing) return;
              if (e.key === 'Enter') submit();
            }}
          />
        </div>
        <div class="modal-footer">
          <button class="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button class="btn btn-primary" onClick={submit} disabled={busy || !name.trim()}>
            {t('rename.confirm')}
          </button>
        </div>
      </div>
    </div>
  );
}

interface DeleteDialogProps {
  /** One or more sessions. A single item keeps the original confirm copy. */
  sessions: SessionMetaWithProject[];
  onClose: () => void;
  /** Called with ids that were actually removed (may be a subset on partial failure). */
  onDone: (deletedIds: string[]) => void;
}

function localizeDeleteError(cause: unknown, t: ReturnType<typeof useT>): string {
  if (cause instanceof DeleteSessionError) {
    const localized = (
      {
        SESSION_IN_USE: t('delete.inUse'),
        SESSION_NOT_FOUND: t('delete.notFound'),
        INVALID_SESSION: t('delete.invalid'),
        DELETE_FAILED: t('delete.failed'),
      } as Record<string, string>
    )[cause.code ?? ''];
    if (localized) return localized;
  }
  return cause instanceof Error ? cause.message : String(cause);
}

export function DeleteDialog({ sessions, onClose, onDone }: DeleteDialogProps) {
  const t = useT();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remaining = sessions;
  const name = remaining[0]?.name || remaining[0]?.id.slice(0, 8) || '';
  const many = remaining.length > 1;
  const body = many
    ? t('delete.bodyMany', { n: remaining.length })
    : t('delete.body', { name });

  async function confirm() {
    if (busy || remaining.length === 0) return;
    setBusy(true);
    setError(null);
    const deleted: string[] = [];
    const failed: { name: string; message: string }[] = [];
    for (const session of remaining) {
      try {
        await deleteSession(session.project_hash, session.id);
        deleted.push(session.id);
      } catch (cause) {
        failed.push({
          name: session.name || session.id.slice(0, 8),
          message: localizeDeleteError(cause, t),
        });
      }
    }
    if (deleted.length > 0) onDone(deleted);
    if (failed.length === 0) {
      onClose();
      return;
    }
    const names = failed.map((item) => item.name).join('、');
    setError(
      failed.length === remaining.length && failed.length === 1
        ? failed[0].message
        : t('delete.partialFailed', { n: failed.length, names }),
    );
    setBusy(false);
  }

  return (
    <div
      class="modal-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div class="modal-card modal-card-sm">
        <div class="modal-header">
          <span>🗑</span>
          <h3>{t('delete.title')}</h3>
          <button class="ghost-btn modal-close" onClick={onClose} aria-label={t('common.cancel')}>
            ×
          </button>
        </div>
        <div class="modal-body">
          <p class="field-hint">{body}</p>
          {error && <div class="modal-error" role="alert">{error}</div>}
        </div>
        <div class="modal-footer">
          <button class="btn" onClick={onClose}>
            {t('common.cancel')}
          </button>
          <button class="btn btn-danger" onClick={confirm} disabled={busy || remaining.length === 0}>
            {t('delete.confirm')}
          </button>
        </div>
      </div>
    </div>
  );
}
