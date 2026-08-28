// Individual settings dialogs: theme, language, and (read-only) model config.
// Each is opened on its own from the sidebar settings menu.

import { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import {
  getConfig,
  ConfigInfo,
  ProviderInfo,
  createProvider,
  updateProvider,
  setDefaultProvider,
  deleteProvider,
  fetchUpstreamModels,
  getTunnelStatus,
  TunnelStatus,
} from '../api';
import { useSettings, Theme } from '../settings';
import { Lang } from '../i18n';
import { ConfirmDialog } from './ConfirmDialog';
import { Select } from './Select';

// AtomGit 托管 provider 的 LLM 网关地址；其上下文窗口由平台固定，前端禁止修改。
const ATOMGIT_BASE_URL = 'https://llm-api.atomgit.com/v1';

// 上下文窗口预设（数值与配置一致，显示时按 /1000 换算为「k tokens」）。
const CONTEXT_WINDOW_PRESETS = [32000, 64000, 128000, 256000, 512000, 1000000];

/** 与 TUI `/provider` 对齐的三大可自定义协议 + ollama。 */
const PROVIDER_TYPE_OPTIONS = [
  { value: 'openai', label: 'openai (Chat Completions)' },
  { value: 'anthropic', label: 'anthropic (Messages)' },
  { value: 'responses', label: 'responses (OpenAI Responses)' },
  { value: 'ollama', label: 'ollama' },
];

const REASONING_EFFORT_OPTIONS = [
  { value: '', label: '（默认）' },
  { value: 'low', label: 'low' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
  { value: 'xhigh', label: 'xhigh' },
  { value: 'max', label: 'max' },
];

const REASONING_HISTORY_OPTIONS = [
  { value: 'include', label: 'include（回传思考）' },
  { value: 'exclude', label: 'exclude（不回传）' },
];

function normalizeProviderType(type: string | undefined): string {
  const t = (type || 'openai').toLowerCase();
  if (t === 'claude') return 'anthropic';
  if (t === 'openai-compatible') return 'openai';
  if (t === 'anthropic-compatible') return 'anthropic';
  if (t === 'responses-compatible') return 'responses';
  return t;
}

/** 把 context_window 数值格式化为下拉标签：1000000 → "1M"，其余 → "<n>K"。 */
function fmtContextWindow(v: number): string {
  return v >= 1000000 ? `${v / 1000000}M` : `${Math.round(v / 1000)}K`;
}

/** Shared modal chrome for the settings dialogs. */
function SettingsModal({
  title,
  wide,
  large,
  hideFooter,
  onClose,
  children,
}: {
  title: string;
  wide?: boolean;
  large?: boolean;
  // 弹窗自带底部操作（如「添加模型」的 关闭/添加）时隐藏这里的页脚关闭，避免重复。
  hideFooter?: boolean;
  onClose: () => void;
  children: ComponentChildren;
}) {
  const { t } = useSettings();
  const sizeClass = large ? ' modal-card-lg' : wide ? '' : ' modal-card-sm';
  return (
    <div
      class="modal-overlay"
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div class={'modal-card' + sizeClass}>
        <div class="modal-header">
          <span>⚙</span>
          <h3>{title}</h3>
          <button class="ghost-btn modal-close" onClick={onClose} aria-label={t('settings.close')}>
            ×
          </button>
        </div>
        <div class="modal-body">{children}</div>
        {!hideFooter && (
          <div class="modal-footer">
            <button class="btn" onClick={onClose}>
              {t('settings.close')}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

export function ThemeDialog({ onClose }: { onClose: () => void }) {
  const { theme, setTheme, t } = useSettings();
  const options: { value: Theme; label: string }[] = [
    { value: 'light', label: t('settings.theme.light') },
    { value: 'dark', label: t('settings.theme.dark') },
    { value: 'system', label: t('settings.theme.system') },
  ];
  return (
    <SettingsModal title={t('settings.menuTheme')} onClose={onClose}>
      <div class="field-group">
        <span class="modal-label">{t('settings.theme')}</span>
        <div class="segmented">
          {options.map((o) => (
            <button
              key={o.value}
              class={'segmented-btn' + (theme === o.value ? ' active' : '')}
              onClick={() => setTheme(o.value)}
              type="button"
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>
    </SettingsModal>
  );
}

export function LanguageDialog({ onClose }: { onClose: () => void }) {
  const { lang, setLang, t } = useSettings();
  const options: { value: Lang; label: string }[] = [
    { value: 'zh', label: '中文' },
    { value: 'en', label: 'English' },
  ];
  return (
    <SettingsModal title={t('settings.menuLang')} onClose={onClose}>
      <div class="field-group">
        <span class="modal-label">{t('settings.language')}</span>
        <div class="segmented">
          {options.map((o) => (
            <button
              key={o.value}
              class={'segmented-btn' + (lang === o.value ? ' active' : '')}
              onClick={() => setLang(o.value)}
              type="button"
            >
              {o.label}
            </button>
          ))}
        </div>
      </div>
    </SettingsModal>
  );
}

export function ModelConfigDialog({ onClose }: { onClose: () => void }) {
  const { t } = useSettings();
  const [config, setConfig] = useState<ConfigInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // 添加模型改为独立弹窗
  const [showAdd, setShowAdd] = useState(false);
  // 编辑已有 provider：选中的条目（null=未编辑）。
  const [editTarget, setEditTarget] = useState<ProviderInfo | null>(null);
  // 删除确认改用 webui 弹窗（ConfirmDialog），不再用系统 confirm/alert。
  const [deleteTarget, setDeleteTarget] = useState<string | null>(null);

  const reload = () =>
    getConfig()
      .then(setConfig)
      .catch((e: unknown) => setLoadError(e instanceof Error ? e.message : String(e)));

  useEffect(() => { reload(); }, []);

  return (
    <>
    <SettingsModal title={t('settings.menuModel')} wide onClose={onClose}>
      <div class="field-group">
        <span class="modal-label">{t('settings.modelConfig')}</span>
        {loadError && <div class="modal-error">{t('settings.loadFailed')}: {loadError}</div>}
        {!config && !loadError && <div class="modal-loading">{t('settings.loading')}</div>}
        {config && (
          <>
            <div class="add-model-top">
              <button
                class="btn btn-primary"
                type="button"
                onClick={() => setShowAdd(true)}
              >
                ＋ {t('settings.addModel')}
              </button>
            </div>
            <Row label={t('settings.defaultProvider')} value={config.default_provider} />
            {config.default_workdir && (
              <Row label={t('settings.defaultWorkdir')} value={config.default_workdir} mono />
            )}
            <Row label={t('settings.configFile')} value={config.path} mono />

            <div class="provider-list">
              <span class="modal-label">
                {t('settings.providers')} ({config.providers.length})
              </span>
              {[...config.providers].sort((a, b) => {
                if (a.is_default !== b.is_default) return a.is_default ? -1 : 1;
                return a.name.localeCompare(b.name);
              }).map((p) => (
                <div key={p.name} class={'provider-card' + (p.is_default ? ' default' : '')}>
                  <div class="provider-card-head">
                    <span class="provider-name">{p.name}</span>
                    {p.is_default && (
                      <span class="provider-default-badge">{t('settings.default')}</span>
                    )}
                    <span class="provider-type">{p.type}</span>
                    {/* AtomGit 托管 provider 由平台固定，禁止编辑（仅保留删除）。 */}
                    {p.base_url !== ATOMGIT_BASE_URL && (
                      <button
                        class="provider-edit-btn"
                        type="button"
                        onClick={() => setEditTarget(p)}
                        title={t('settings.edit')}
                      >
                        {t('settings.edit')}
                      </button>
                    )}
                    <button
                      class="provider-delete-btn"
                      type="button"
                      onClick={() => setDeleteTarget(p.name)}
                      title={t('settings.delete')}
                    >
                      {t('settings.delete')}
                    </button>
                  </div>
                  <div class="provider-card-body">
                    <div>
                      <span class="pk">{t('settings.model')}: </span>
                      <span class="pv">{p.model}</span>
                    </div>
                    {p.base_url && (
                      <div>
                        <span class="pk">base_url: </span>
                        <span class="pv">{p.base_url}</span>
                      </div>
                    )}
                    {p.context_window && (
                      <div>
                        <span class="pk">{t('settings.contextWindow')}: </span>
                        <span>{(p.context_window / 1000).toFixed(0)}k tokens</span>
                      </div>
                    )}
                    {p.base_url !== ATOMGIT_BASE_URL && (
                      <div>
                        <span class="pk">{t('settings.apiKey')}: </span>
                        <span class={p.has_api_key ? 'ok' : 'nok'}>
                          {p.has_api_key ? t('settings.configured') : t('settings.notConfigured')}
                        </span>
                      </div>
                    )}
                    <div>
                      <span class="pk">{t('settings.supportsVision')}: </span>
                      <span>{p.supports_vision ? t('settings.yes') : t('settings.no')}</span>
                      <span class="pk"> · {t('settings.reasoningModel')}: </span>
                      <span>{p.reasoning_model ? t('settings.yes') : t('settings.no')}</span>
                      {p.reasoning_model && p.reasoning_effort && (
                        <>
                          <span class="pk"> · {t('settings.reasoningEffort')}: </span>
                          <span class="pv">{p.reasoning_effort}</span>
                        </>
                      )}
                      {p.reasoning_model && p.reasoning_history && (
                        <>
                          <span class="pk"> · {t('settings.reasoningHistory')}: </span>
                          <span class="pv">{p.reasoning_history}</span>
                        </>
                      )}
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </>
        )}
      </div>
    </SettingsModal>
    {showAdd && (
      <ProviderFormDialog
        existingNames={config?.providers.map((p) => p.name) ?? []}
        onClose={() => setShowAdd(false)}
        onSaved={() => {
          setShowAdd(false);
          reload();
        }}
      />
    )}
    {editTarget && (
      <ProviderFormDialog
        editing={editTarget}
        existingNames={config?.providers.map((p) => p.name) ?? []}
        onClose={() => setEditTarget(null)}
        onSaved={() => {
          setEditTarget(null);
          reload();
        }}
      />
    )}
    {deleteTarget && (
      <ConfirmDialog
        title={t('settings.deleteTitle')}
        body={t('settings.deleteConfirm', { name: deleteTarget })}
        confirmLabel={t('settings.delete')}
        cancelLabel={t('common.cancel')}
        onConfirm={async () => {
          await deleteProvider(deleteTarget);
          reload();
        }}
        onClose={() => setDeleteTarget(null)}
      />
    )}
    </>
  );
}

/**
 * 「添加 / 编辑模型」弹窗 — 对齐 TUI `/provider`：
 * openai / anthropic / responses；图片输入；思考模型 + 档位 + 是否回传思考；
 * 模型 ID 内嵌筛选框 + 右侧刷新拉取上游列表。
 */
function ProviderFormDialog({
  editing,
  existingNames = [],
  onClose,
  onSaved,
}: {
  editing?: ProviderInfo;
  existingNames?: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useSettings();
  const isEdit = !!editing;
  const [name] = useState(editing?.name ?? '');
  const [nameInput, setNameInput] = useState(editing?.name ?? '');
  const [type, setType] = useState(normalizeProviderType(editing?.type));
  const [model, setModel] = useState(editing?.model ?? '');
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? '');
  const [apiKey, setApiKey] = useState('');
  const [contextWindow, setContextWindow] = useState<number>(editing?.context_window ?? 128000);
  const [supportsVision, setSupportsVision] = useState(Boolean(editing?.supports_vision));
  const [reasoningModel, setReasoningModel] = useState(Boolean(editing?.reasoning_model));
  const [reasoningEffort, setReasoningEffort] = useState(editing?.reasoning_effort ?? '');
  const [reasoningHistory, setReasoningHistory] = useState(
    editing?.reasoning_history === 'exclude' ? 'exclude' : 'include',
  );
  const [setDefault, setSetDefault] = useState(editing?.is_default ?? false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [candidates, setCandidates] = useState<string[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchStatus, setFetchStatus] = useState<string | null>(null);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [highlight, setHighlight] = useState(0);
  const modelWrapRef = useRef<HTMLDivElement | null>(null);

  const isAtomGit = editing?.base_url === ATOMGIT_BASE_URL;
  const cwOptions = CONTEXT_WINDOW_PRESETS.includes(contextWindow)
    ? CONTEXT_WINDOW_PRESETS
    : [contextWindow, ...CONTEXT_WINDOW_PRESETS];

  const filtered = useMemo(() => {
    const q = model.trim().toLowerCase();
    if (!q) return candidates;
    return candidates.filter((id) => id.toLowerCase().includes(q));
  }, [candidates, model]);

  useEffect(() => {
    setHighlight(0);
  }, [model, candidates]);

  useEffect(() => {
    if (!modelMenuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (modelWrapRef.current && !modelWrapRef.current.contains(e.target as Node)) {
        setModelMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    return () => document.removeEventListener('mousedown', onDown);
  }, [modelMenuOpen]);

  const refreshUpstream = async () => {
    if (!baseUrl.trim()) {
      setFetchStatus(t('settings.upstreamNeedBaseUrl'));
      return;
    }
    setFetching(true);
    setFetchStatus(t('settings.upstreamFetching'));
    setModelMenuOpen(true);
    try {
      const models = await fetchUpstreamModels({
        protocol: type,
        base_url: baseUrl.trim(),
        api_key: apiKey.trim() || undefined,
        provider_name: isEdit ? name : undefined,
      });
      setCandidates(models);
      setFetchStatus(
        models.length
          ? t('settings.upstreamLoaded', { n: models.length })
          : t('settings.upstreamEmpty'),
      );
    } catch (e: unknown) {
      setCandidates([]);
      setFetchStatus(
        `${t('settings.upstreamFailed')}: ${e instanceof Error ? e.message : String(e)}`,
      );
    } finally {
      setFetching(false);
    }
  };

  const pickModel = (id: string) => {
    setModel(id);
    setModelMenuOpen(false);
  };

  const handleSave = async () => {
    const newName = nameInput.trim();
    if (isEdit) {
      if (!newName || !model.trim() || !baseUrl.trim()) {
        setError(t('settings.allRequired'));
        return;
      }
    } else if (!newName || !model.trim() || !baseUrl.trim() || !apiKey.trim()) {
      setError(t('settings.allRequired'));
      return;
    }
    if (
      existingNames.some(
        (n) => n.toLowerCase() !== name.toLowerCase() && n.toLowerCase() === newName.toLowerCase(),
      )
    ) {
      setError(t('settings.nameExists'));
      return;
    }
    setSaving(true);
    setError(null);
    const advanced = {
      supports_vision: supportsVision,
      reasoning_model: reasoningModel,
      reasoning_effort: reasoningModel && reasoningEffort ? reasoningEffort : null,
      reasoning_history: reasoningModel ? reasoningHistory : null,
    };
    try {
      if (isEdit) {
        await updateProvider(name, {
          ...(newName !== name ? { name: newName } : {}),
          type,
          model: model.trim(),
          base_url: baseUrl.trim(),
          ...(apiKey.trim() ? { api_key: apiKey.trim() } : {}),
          ...(isAtomGit ? {} : { context_window: contextWindow }),
          ...advanced,
        });
        if (setDefault && !editing?.is_default) {
          await setDefaultProvider(newName);
        }
      } else {
        await createProvider({
          name: newName,
          type,
          model: model.trim(),
          base_url: baseUrl.trim(),
          api_key: apiKey.trim(),
          context_window: contextWindow,
          set_default: setDefault || undefined,
          ...advanced,
        });
      }
      onSaved();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingsModal
      title={isEdit ? t('settings.editModel') : t('settings.addModel')}
      large
      hideFooter
      onClose={onClose}
    >
      <div class="field-group add-model-form">
        <div class="add-model-field">
          <label class="add-model-label">{t('settings.providerName')}</label>
          <input
            class="menu-input"
            type="text"
            placeholder="my-deepseek"
            value={nameInput}
            onInput={(e) => setNameInput((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-field" ref={modelWrapRef}>
          <label class="add-model-label">{t('settings.modelId')}</label>
          <div class="model-id-row">
            <input
              class="menu-input model-id-input"
              type="text"
              placeholder={t('settings.modelIdPlaceholder')}
              value={model}
              onFocus={() => setModelMenuOpen(true)}
              onInput={(e) => {
                setModel((e.target as HTMLInputElement).value);
                setModelMenuOpen(true);
              }}
              onKeyDown={(e) => {
                if (!modelMenuOpen || filtered.length === 0) return;
                if (e.key === 'ArrowDown') {
                  e.preventDefault();
                  setHighlight((h) => Math.min(h + 1, filtered.length - 1));
                } else if (e.key === 'ArrowUp') {
                  e.preventDefault();
                  setHighlight((h) => Math.max(h - 1, 0));
                } else if (e.key === 'Enter' && filtered[highlight]) {
                  e.preventDefault();
                  pickModel(filtered[highlight]!);
                } else if (e.key === 'Escape') {
                  setModelMenuOpen(false);
                }
              }}
            />
            <button
              type="button"
              class="btn model-id-refresh"
              disabled={fetching}
              title={t('settings.upstreamRefresh')}
              onClick={() => void refreshUpstream()}
            >
              {fetching ? '…' : '↻'}
            </button>
          </div>
          {fetchStatus && <span class="field-hint">{fetchStatus}</span>}
          {modelMenuOpen && filtered.length > 0 && (
            <div class="model-id-menu" role="listbox">
              {filtered.slice(0, 80).map((id, i) => (
                <button
                  key={id}
                  type="button"
                  class={'model-id-option' + (i === highlight ? ' active' : '')}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    pickModel(id);
                  }}
                  onMouseEnter={() => setHighlight(i)}
                >
                  {id}
                </button>
              ))}
            </div>
          )}
        </div>

        <div class="add-model-row">
          <div class="add-model-field add-model-field-type">
            <label class="add-model-label">{t('settings.providerType')}</label>
            <Select
              value={type}
              options={PROVIDER_TYPE_OPTIONS}
              onChange={(v) => setType(v)}
            />
          </div>
          <div class="add-model-field add-model-field-default">
            <label class="add-model-checkbox-label">
              <input
                type="checkbox"
                checked={setDefault}
                disabled={editing?.is_default}
                onChange={(e) => setSetDefault((e.target as HTMLInputElement).checked)}
              />
              {t('settings.setAsDefault')}
            </label>
          </div>
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.contextWindow')}</label>
          <Select
            value={String(contextWindow)}
            disabled={isAtomGit}
            options={cwOptions.map((v) => ({
              value: String(v),
              label: `${fmtContextWindow(v)} tokens`,
            }))}
            onChange={(v) => setContextWindow(Number(v))}
          />
          {isAtomGit && (
            <span class="field-hint">{t('settings.contextWindowLocked')}</span>
          )}
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.baseUrl')}</label>
          <input
            class="menu-input"
            type="text"
            placeholder="https://api.example.com/v1"
            value={baseUrl}
            onInput={(e) => setBaseUrl((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.apiKeyInput')}</label>
          <input
            class="menu-input"
            type="password"
            placeholder={isEdit ? t('settings.apiKeyKeep') : 'sk-...'}
            value={apiKey}
            onInput={(e) => setApiKey((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-toggles">
          <label class="add-model-checkbox-label">
            <input
              type="checkbox"
              checked={supportsVision}
              onChange={(e) => setSupportsVision((e.target as HTMLInputElement).checked)}
            />
            {t('settings.supportsVision')}
          </label>
          <label class="add-model-checkbox-label">
            <input
              type="checkbox"
              checked={reasoningModel}
              onChange={(e) => setReasoningModel((e.target as HTMLInputElement).checked)}
            />
            {t('settings.reasoningModel')}
          </label>
        </div>

        {reasoningModel && (
          <div class="add-model-row">
            <div class="add-model-field">
              <label class="add-model-label">{t('settings.reasoningEffort')}</label>
              <Select
                value={reasoningEffort}
                options={REASONING_EFFORT_OPTIONS.map((o) => ({
                  value: o.value,
                  label: o.value ? o.label : t('settings.effortDefault'),
                }))}
                onChange={setReasoningEffort}
              />
            </div>
            <div class="add-model-field">
              <label class="add-model-label">{t('settings.reasoningHistory')}</label>
              <Select
                value={reasoningHistory}
                options={REASONING_HISTORY_OPTIONS.map((o) => ({
                  value: o.value,
                  label:
                    o.value === 'include'
                      ? t('settings.historyInclude')
                      : t('settings.historyExclude'),
                }))}
                onChange={setReasoningHistory}
              />
            </div>
          </div>
        )}

        {error && (
          <div class="modal-error">
            {(isEdit ? t('settings.updateFailed') : t('settings.addFailed'))}: {error}
          </div>
        )}
        <div class="add-model-actions">
          <button class="btn" type="button" onClick={onClose}>
            {t('settings.close')}
          </button>
          <button class="btn btn-primary" type="button" disabled={saving} onClick={handleSave}>
            {isEdit
              ? saving
                ? t('settings.saving')
                : t('settings.save')
              : saving
                ? t('settings.adding')
                : t('settings.add')}
          </button>
        </div>
      </div>
    </SettingsModal>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div class="config-row">
      <span class="config-key">{label}</span>
      <span class={'config-val' + (mono ? ' mono' : '')}>{value}</span>
    </div>
  );
}

/** 远程访问（蒲公英 / Oray PGY）：检测状态，给出可扫码的私网 URL。 */
export function RemoteAccessDialog({ onClose }: { onClose: () => void }) {
  const { t, lang } = useSettings();
  const [status, setStatus] = useState<TunnelStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [copied, setCopied] = useState(false);

  const reload = () => {
    setLoading(true);
    getTunnelStatus()
      .then(setStatus)
      .catch(() => setStatus(null))
      .finally(() => setLoading(false));
  };
  useEffect(() => { reload(); }, []);

  const pgy = status?.pgy;
  // 服务端未给 remote_url（绑回环）时，展示一个「示意」地址。注意：token 现在只存在
  // 于 HttpOnly Cookie 中，前端 JS 读不到（防插件拦截，CWE-598），所以这里无法拼出
  // 可直接登录的链接——要可分享的真实链接需把 webui 绑到局域网，由服务端下发
  // remote_url（带 token）。回环示意地址因此不带 token。
  const fallbackUrl =
    pgy?.ipv4 && status
      ? `http://${pgy.ipv4}:${status.port}/?sync=1`
      : null;

  function copy() {
    const url = status?.remote_url ?? fallbackUrl;
    if (!url) return;
    navigator.clipboard?.writeText(url).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }

  return (
    <SettingsModal title={t('remote.title')} onClose={onClose}>
      <div class="field-group remote-access">
        <p class="field-hint">{t('remote.intro')}</p>

        {loading && <div class="modal-loading">{t('remote.loading')}</div>}

        {!loading && status && (
          <>
            {/* 1) 未装 / 未连蒲公英 */}
            {(!pgy?.installed || !pgy?.ipv4) && (
              <div class="remote-state">
                <p>{pgy?.installed ? t('remote.notConnected') : t('remote.notInstalled')}</p>
                <a
                  class="btn btn-primary"
                  href="https://pgy.oray.com"
                  target="_blank"
                  rel="noreferrer"
                >
                  {t('remote.installLink')}
                </a>
              </div>
            )}

            {/* 2) 已装+有 IP，但 server 仅绑回环 → 提示改绑 */}
            {pgy?.installed && pgy?.ipv4 && !status.remote_url && (
              <div class="remote-state">
                <p>{t('remote.notReachable', { ip: pgy.ipv4 })}</p>
                {fallbackUrl && <code class="remote-url">{fallbackUrl}</code>}
              </div>
            )}

            {/* 3) 就绪：二维码 + URL */}
            {status.remote_url && (
              <div class="remote-state remote-ready">
                <p>{t('remote.ready')}</p>
                {status.qr_svg && (
                  <div
                    class="remote-qr"
                    // eslint-disable-next-line react/no-danger
                    dangerouslySetInnerHTML={{ __html: status.qr_svg }}
                  />
                )}
                <code class="remote-url">{status.remote_url}</code>
                <div class="remote-actions">
                  <button class="btn" onClick={copy}>
                    {copied ? t('remote.copied') : t('remote.copy')}
                  </button>
                </div>
                <p class="field-hint remote-warn">⚠️ {t('remote.warnToken')}</p>
              </div>
            )}
          </>
        )}

        <div class="remote-actions">
          <button class="btn" onClick={reload} disabled={loading}>
            {t('remote.refresh')}
          </button>
          {/* 使用引导：跳官网对应语言的说明页，新标签打开。 */}
          <a
            class="btn"
            href={`https://atomcode.atomgit.com/docs/${lang}/webui-remote-access.html`}
            target="_blank"
            rel="noreferrer"
          >
            {t('remote.guide')}
          </a>
        </div>
      </div>
    </SettingsModal>
  );
}
