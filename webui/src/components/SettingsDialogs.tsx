// Individual settings dialogs: theme, language, and (read-only) model config.
// Each is opened on its own from the sidebar settings menu.

import { ComponentChildren } from 'preact';
import { useEffect, useMemo, useRef, useState } from 'preact/hooks';
import {
  getConfig,
  ConfigInfo,
  ProviderInfo,
  AccountInfo,
  createOrUpdateAccount,
  deleteAccount,
  createProvider,
  updateProvider,
  setDefaultProvider,
  deleteProvider,
  fetchUpstreamModels,
} from '../api';
import { useSettings, Theme } from '../settings';
import { Lang } from '../i18n';
import { ConfirmDialog } from './ConfirmDialog';
import { Select } from './Select';

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
  extraLarge,
  hideFooter,
  onClose,
  children,
}: {
  title: string;
  wide?: boolean;
  large?: boolean;
  extraLarge?: boolean;
  // 弹窗自带底部操作（如「添加模型」的 关闭/添加）时隐藏这里的页脚关闭，避免重复。
  hideFooter?: boolean;
  onClose: () => void;
  children: ComponentChildren;
}) {
  const { t } = useSettings();
  const sizeClass = extraLarge
    ? ' modal-card-xl'
    : large
    ? ' modal-card-lg'
    : wide
    ? ''
    : ' modal-card-sm';
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

/** 提供商账号编辑弹窗 */
function AccountFormDialog({
  editing,
  existingIds = [],
  onClose,
  onSaved,
}: {
  editing?: { id: string; type: string; base_url?: string; has_api_key: boolean; skip_tls_verify?: boolean };
  existingIds?: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useSettings();
  const isEdit = !!editing;
  const [id, setId] = useState(editing?.id ?? '');
  const [type, setType] = useState(normalizeProviderType(editing?.type));
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? '');
  const [apiKey, setApiKey] = useState('');
  const [skipTls, setSkipTls] = useState(Boolean(editing?.skip_tls_verify));
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleSave = async () => {
    const trimmedId = id.trim();
    if (!trimmedId) {
      setError(t('settings.nameModelRequired'));
      return;
    }
    if (!isEdit && existingIds.includes(trimmedId)) {
      setError(t('settings.nameExists'));
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await createOrUpdateAccount(trimmedId, {
        id: trimmedId,
        type,
        base_url: baseUrl.trim() || undefined,
        api_key: apiKey.trim() || undefined,
        skip_tls_verify: skipTls,
      });
      onSaved();
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <SettingsModal
      title={isEdit ? t('settings.editAccount') : t('settings.addAccount')}
      large
      hideFooter
      onClose={onClose}
    >
      <div class="field-group add-model-form">
        <div class="add-model-field">
          <label class="add-model-label">{t('settings.accountName')}</label>
          <input
            class="menu-input"
            type="text"
            placeholder="openai / deepseek / gemini"
            disabled={isEdit}
            value={id}
            onInput={(e) => setId((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.providerType')}</label>
          <Select
            value={type}
            options={PROVIDER_TYPE_OPTIONS}
            onChange={(v) => setType(v)}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.baseUrl')}</label>
          <input
            class="menu-input"
            type="text"
            placeholder="https://api.openai.com/v1"
            value={baseUrl}
            onInput={(e) => setBaseUrl((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.apiKeyInput')}</label>
          <input
            class="menu-input"
            type="password"
            placeholder={isEdit ? t('settings.apiKeyKeep') : 'sk-…'}
            value={apiKey}
            onInput={(e) => setApiKey((e.target as HTMLInputElement).value)}
          />
        </div>

        {error && <div class="modal-error">{error}</div>}

        <div class="modal-footer" style={{ marginTop: '16px', padding: 0 }}>
          <button class="btn" type="button" onClick={onClose} disabled={saving}>
            {t('settings.close')}
          </button>
          <button class="btn btn-primary" type="button" onClick={handleSave} disabled={saving}>
            {saving ? t('settings.saving') : t('settings.save')}
          </button>
        </div>
      </div>
    </SettingsModal>
  );
}

export function ModelConfigDialog({ onClose }: { onClose: () => void }) {
  const { t } = useSettings();
  const [config, setConfig] = useState<ConfigInfo | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);

  // 折叠状态（按 accountId 控制，默认全部展开）
  const [collapsedAccounts, setCollapsedAccounts] = useState<Record<string, boolean>>({});

  // 弹窗状态
  const [showAddModel, setShowAddModel] = useState(false);
  const [addModelDefaultAccount, setAddModelDefaultAccount] = useState<string | undefined>(undefined);
  const [editModelTarget, setEditModelTarget] = useState<ProviderInfo | null>(null);
  const [deleteModelTarget, setDeleteModelTarget] = useState<string | null>(null);

  const [editAccountTarget, setEditAccountTarget] = useState<{ id: string; type: string; base_url?: string; has_api_key: boolean; skip_tls_verify?: boolean } | null>(null);
  const [showAddAccount, setShowAddAccount] = useState(false);
  const [deleteAccountTarget, setDeleteAccountTarget] = useState<string | null>(null);

  const reload = () =>
    getConfig()
      .then(setConfig)
      .catch((e: unknown) => setLoadError(e instanceof Error ? e.message : String(e)));

  useEffect(() => { reload(); }, []);

  const toggleCollapse = (accId: string) => {
    setCollapsedAccounts((prev) => ({ ...prev, [accId]: !prev[accId] }));
  };

  // 按提供商账号归类模型
  const accountGroups = useMemo(() => {
    if (!config) return [];
    const accounts = config.accounts ?? [];
    const providers = config.providers ?? [];

    const groupMap = new Map<
      string,
      {
        account: { id: string; type: string; base_url?: string; has_api_key: boolean; skip_tls_verify?: boolean };
        models: ProviderInfo[];
      }
    >();

    // 先填入所有已知账号
    for (const acc of accounts) {
      groupMap.set(acc.id, { account: acc, models: [] });
    }

    // 归入模型
    for (const p of providers) {
      const accId = p.account || p.name;
      if (!groupMap.has(accId)) {
        groupMap.set(accId, {
          account: {
            id: accId,
            type: p.type,
            base_url: p.base_url,
            has_api_key: p.has_api_key,
            skip_tls_verify: p.skip_tls_verify,
          },
          models: [],
        });
      }
      groupMap.get(accId)!.models.push(p);
    }

    // 排序：有默认模型的账号在前，其余按名称字母排
    return Array.from(groupMap.values()).sort((a, b) => {
      const aHasDef = a.models.some((m) => m.is_default);
      const bHasDef = b.models.some((m) => m.is_default);
      if (aHasDef !== bHasDef) return aHasDef ? -1 : 1;
      return a.account.id.localeCompare(b.account.id);
    });
  }, [config]);

  const [searchQuery, setSearchQuery] = useState('');

  const filteredAccountGroups = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    if (!q) return accountGroups;
    return accountGroups
      .map((group) => {
        const accMatch =
          group.account.id.toLowerCase().includes(q) ||
          group.account.type.toLowerCase().includes(q) ||
          (group.account.base_url && group.account.base_url.toLowerCase().includes(q));
        const matchingModels = group.models.filter(
          (m) =>
            m.name.toLowerCase().includes(q) ||
            m.model.toLowerCase().includes(q) ||
            m.type.toLowerCase().includes(q)
        );
        if (accMatch) return group;
        if (matchingModels.length > 0) {
          return { ...group, models: matchingModels };
        }
        return null;
      })
      .filter((g): g is NonNullable<typeof g> => g !== null);
  }, [accountGroups, searchQuery]);

  const allCollapsed =
    filteredAccountGroups.length > 0 &&
    filteredAccountGroups.every((g) => Boolean(collapsedAccounts[g.account.id]));

  const toggleAllCollapse = () => {
    const next: Record<string, boolean> = {};
    const target = !allCollapsed;
    for (const g of accountGroups) {
      next[g.account.id] = target;
    }
    setCollapsedAccounts(next);
  };

  return (
    <>
    <SettingsModal title={t('settings.menuModel')} extraLarge onClose={onClose}>
      <div class="field-group" style={{ gap: '14px' }}>
        {loadError && <div class="modal-error">{t('settings.loadFailed')}: {loadError}</div>}
        {!config && !loadError && <div class="modal-loading">{t('settings.loading')}</div>}
        {config && (
          <>
            {/* Overview / Header Card */}
            <div class="model-config-overview">
              <div class="model-config-overview-top">
                <div class="model-config-overview-info">
                  <div class="model-config-overview-title">
                    <span>⚡</span>
                    <span>{t('settings.menuModel')}</span>
                  </div>
                  <div class="model-config-overview-desc">
                    {t('settings.modelConfigSubtitle')}
                  </div>
                </div>

                <div class="model-config-overview-actions">
                  <button
                    class="btn btn-primary"
                    type="button"
                    onClick={() => {
                      setAddModelDefaultAccount(undefined);
                      setShowAddModel(true);
                    }}
                  >
                    ＋ {t('settings.addModel')}
                  </button>
                  <button
                    class="btn"
                    type="button"
                    onClick={() => setShowAddAccount(true)}
                  >
                    ＋ {t('settings.addAccount')}
                  </button>
                </div>
              </div>

              <div class="model-config-overview-stats">
                <div class="model-config-stat-chip default-provider" title={t('settings.activeDefaultModel')}>
                  <span>🎯 {t('settings.activeDefaultModel')}:</span>
                  <strong>{config.default_provider || '（未设置）'}</strong>
                </div>
                <div class="model-config-stat-chip">
                  <span>🏢</span>
                  <span>{t('settings.statsProviders', { n: accountGroups.length })}</span>
                </div>
                <div class="model-config-stat-chip">
                  <span>🤖</span>
                  <span>{t('settings.statsModels', { n: config.providers?.length ?? 0 })}</span>
                </div>
                {config.path && (
                  <div class="model-config-stat-chip config-path" title={config.path}>
                    <span>📄</span>
                    <span>{config.path}</span>
                  </div>
                )}
              </div>
            </div>

            {/* Toolbar: Search + Controls */}
            <div class="model-config-toolbar">
              <div class="model-config-search-box">
                <span class="model-config-search-icon">🔍</span>
                <input
                  class="model-config-search-input"
                  type="text"
                  placeholder={t('settings.searchPlaceholder')}
                  value={searchQuery}
                  onInput={(e) => setSearchQuery((e.target as HTMLInputElement).value)}
                />
                {searchQuery && (
                  <button
                    class="model-config-search-clear"
                    type="button"
                    onClick={() => setSearchQuery('')}
                    title="Clear"
                  >
                    ×
                  </button>
                )}
              </div>

              <div class="model-config-toolbar-actions">
                <button
                  class="provider-group-btn"
                  type="button"
                  onClick={toggleAllCollapse}
                >
                  {allCollapsed ? `▼ ${t('settings.expandAll')}` : `▲ ${t('settings.collapseAll')}`}
                </button>
              </div>
            </div>

            {/* Providers & Models List */}
            <div class="provider-list">
              {filteredAccountGroups.length === 0 && (
                <div class="model-config-empty">
                  <span style={{ fontSize: '26px' }}>🔍</span>
                  <span>{t('settings.noSearchResults')}</span>
                  {searchQuery && (
                    <button
                      class="provider-group-btn"
                      type="button"
                      onClick={() => setSearchQuery('')}
                      style={{ marginTop: '6px' }}
                    >
                      {t('settings.resetSearch')}
                    </button>
                  )}
                </div>
              )}

              {filteredAccountGroups.map(({ account, models }) => {
                const isCollapsed = Boolean(collapsedAccounts[account.id]);
                return (
                  <div key={account.id} class="provider-group">
                    <div
                      class="provider-group-header"
                      onClick={() => toggleCollapse(account.id)}
                    >
                      <div class="provider-group-info">
                        <span class={`provider-group-toggle ${!isCollapsed ? 'open' : ''}`}>
                          ▶
                        </span>
                        <span class="provider-group-title" title={account.id}>
                          {account.id}
                        </span>
                        <span class="provider-group-badge">{account.type}</span>
                        {account.base_url && (
                          <span class="provider-group-url" title={account.base_url}>
                            🔗 {account.base_url}
                          </span>
                        )}
                      </div>

                      <div class="provider-group-meta-actions" onClick={(e) => e.stopPropagation()}>
                        <span class={`provider-status-badge ${account.has_api_key ? 'ok' : 'nok'}`}>
                          <span style={{ fontSize: '9px' }}>●</span>
                          <span>{account.has_api_key ? t('settings.apiKeyConfigured') : t('settings.apiKeyNotConfigured')}</span>
                        </span>
                        <span class="provider-group-count">
                          {t('settings.modelsCount', { n: models.length })}
                        </span>

                        <div class="provider-group-actions">
                          <button
                            class="provider-group-btn btn-accent"
                            type="button"
                            onClick={() => {
                              setAddModelDefaultAccount(account.id);
                              setShowAddModel(true);
                            }}
                            title={t('settings.addModel')}
                          >
                            ＋ {t('settings.model')}
                          </button>
                          <button
                            class="provider-group-btn"
                            type="button"
                            onClick={() => setEditAccountTarget(account)}
                            title={t('settings.edit')}
                          >
                            {t('settings.edit')}
                          </button>
                          <button
                            class="provider-group-btn danger"
                            type="button"
                            onClick={() => setDeleteAccountTarget(account.id)}
                            title={t('settings.delete')}
                          >
                            {t('settings.delete')}
                          </button>
                        </div>
                      </div>
                    </div>

                    {!isCollapsed && (
                      <div class="provider-group-models">
                        {models.length === 0 ? (
                          <div class="provider-group-empty">
                            <span>ℹ️</span>
                            <span>{t('settings.noModels')}</span>
                          </div>
                        ) : (
                          models
                            .sort((a, b) => {
                              if (a.is_default !== b.is_default) return a.is_default ? -1 : 1;
                              return a.name.localeCompare(b.name);
                            })
                            .map((p) => (
                              <div
                                key={p.name}
                                class={`model-card ${p.is_default ? 'default' : ''}`}
                              >
                                <div class="model-card-head">
                                  <div class="model-card-name-row">
                                    <span class="model-card-name">{p.name}</span>
                                    {p.is_default && (
                                      <span class="model-card-badge">★ {t('settings.default')}</span>
                                    )}
                                    <span class="model-card-id" title={p.model}>ID: {p.model}</span>
                                  </div>
                                  <div class="model-card-actions">
                                    {!p.is_default && (
                                      <button
                                        class="provider-group-btn btn-accent"
                                        type="button"
                                        onClick={async () => {
                                          await setDefaultProvider(p.name);
                                          reload();
                                        }}
                                        title={t('settings.setAsDefault')}
                                      >
                                        {t('settings.setAsDefault')}
                                      </button>
                                    )}
                                    <button
                                      class="provider-group-btn"
                                      type="button"
                                      onClick={() => setEditModelTarget(p)}
                                      title={t('settings.edit')}
                                    >
                                      {t('settings.edit')}
                                    </button>
                                    <button
                                      class="provider-group-btn danger"
                                      type="button"
                                      onClick={() => setDeleteModelTarget(p.name)}
                                      title={t('settings.delete')}
                                    >
                                      {t('settings.delete')}
                                    </button>
                                  </div>
                                </div>

                                <div class="model-card-meta">
                                  {p.context_window && (
                                    <div class="model-card-meta-item">
                                      <span class="pk">⚡ {t('settings.contextWindow')}:</span>
                                      <span class="pv">{(p.context_window / 1000).toFixed(0)}k</span>
                                    </div>
                                  )}
                                  <div class="model-card-meta-item">
                                    <span class="pk">🖼️ {t('settings.supportsVision')}:</span>
                                    <span class="pv">{p.supports_vision ? t('settings.yes') : t('settings.no')}</span>
                                  </div>
                                  <div class="model-card-meta-item">
                                    <span class="pk">🧠 {t('settings.reasoningModel')}:</span>
                                    <span class="pv">{p.reasoning_model ? t('settings.yes') : t('settings.no')}</span>
                                    {p.reasoning_model && p.reasoning_effort && (
                                      <span class="pv" style={{ marginLeft: '4px' }}>({p.reasoning_effort})</span>
                                    )}
                                  </div>
                                  {p.reasoning_history && (
                                    <div class="model-card-meta-item">
                                      <span class="pk">💭 {t('settings.reasoningHistory')}:</span>
                                      <span class="pv">{p.reasoning_history}</span>
                                    </div>
                                  )}
                                </div>
                              </div>
                            ))
                        )}
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          </>
        )}
      </div>
    </SettingsModal>

    {showAddModel && (
      <ProviderFormDialog
        defaultAccount={addModelDefaultAccount}
        accounts={config?.accounts ?? []}
        existingNames={config?.providers.map((p) => p.name) ?? []}
        onClose={() => {
          setShowAddModel(false);
          setAddModelDefaultAccount(undefined);
        }}
        onSaved={() => {
          setShowAddModel(false);
          setAddModelDefaultAccount(undefined);
          reload();
        }}
      />
    )}

    {editModelTarget && (
      <ProviderFormDialog
        editing={editModelTarget}
        accounts={config?.accounts ?? []}
        existingNames={config?.providers.map((p) => p.name) ?? []}
        onClose={() => setEditModelTarget(null)}
        onSaved={() => {
          setEditModelTarget(null);
          reload();
        }}
      />
    )}

    {showAddAccount && (
      <AccountFormDialog
        existingIds={(config?.accounts ?? []).map((a) => a.id)}
        onClose={() => setShowAddAccount(false)}
        onSaved={() => {
          setShowAddAccount(false);
          reload();
        }}
      />
    )}

    {editAccountTarget && (
      <AccountFormDialog
        editing={editAccountTarget}
        existingIds={(config?.accounts ?? []).map((a) => a.id)}
        onClose={() => setEditAccountTarget(null)}
        onSaved={() => {
          setEditAccountTarget(null);
          reload();
        }}
      />
    )}

    {deleteModelTarget && (
      <ConfirmDialog
        title={t('settings.deleteTitle')}
        body={t('settings.deleteConfirm', { name: deleteModelTarget })}
        confirmLabel={t('settings.delete')}
        cancelLabel={t('common.cancel')}
        onConfirm={async () => {
          await deleteProvider(deleteModelTarget);
          reload();
        }}
        onClose={() => setDeleteModelTarget(null)}
      />
    )}

    {deleteAccountTarget && (
      <ConfirmDialog
        title={t('settings.deleteAccountTitle')}
        body={t('settings.deleteAccountConfirm', { name: deleteAccountTarget })}
        confirmLabel={t('settings.delete')}
        cancelLabel={t('common.cancel')}
        onConfirm={async () => {
          await deleteAccount(deleteAccountTarget);
          reload();
        }}
        onClose={() => setDeleteAccountTarget(null)}
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
  defaultAccount,
  accounts = [],
  existingNames = [],
  onClose,
  onSaved,
}: {
  editing?: ProviderInfo;
  defaultAccount?: string;
  accounts?: AccountInfo[];
  existingNames?: string[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { t } = useSettings();
  const isEdit = !!editing;

  // 所属提供商账号
  const initialAccount = editing?.account || defaultAccount || (accounts.length > 0 ? accounts[0].id : '');
  const [account, setAccount] = useState<string>(initialAccount);
  const selectedAccount = accounts.find((a) => a.id === account);

  const [name] = useState(editing?.name ?? '');
  const [nameInput, setNameInput] = useState(editing?.name ?? '');
  const [type, setType] = useState(normalizeProviderType(editing?.type || selectedAccount?.type));
  const [model, setModel] = useState(editing?.model ?? '');
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? selectedAccount?.base_url ?? '');
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

  // 当切换所属账号时自动复用账号的配置
  const handleAccountChange = (newAccId: string) => {
    setAccount(newAccId);
    const acc = accounts.find((a) => a.id === newAccId);
    if (acc) {
      setType(normalizeProviderType(acc.type));
      if (acc.base_url) setBaseUrl(acc.base_url);
    }
  };

  const [candidates, setCandidates] = useState<string[]>([]);
  const [fetching, setFetching] = useState(false);
  const [fetchStatus, setFetchStatus] = useState<string | null>(null);
  const [modelMenuOpen, setModelMenuOpen] = useState(false);
  const [highlight, setHighlight] = useState(0);
  const modelWrapRef = useRef<HTMLDivElement | null>(null);

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
    if (!nameInput.trim()) {
      setNameInput(id);
    }
    setModelMenuOpen(false);
  };

  const handleSave = async () => {
    const newName = nameInput.trim();
    if (isEdit) {
      if (!newName || !model.trim()) {
        setError(t('settings.allRequired'));
        return;
      }
    } else if (!newName || !model.trim()) {
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
          account: account.trim() || undefined,
          base_url: baseUrl.trim() || undefined,
          ...(apiKey.trim() ? { api_key: apiKey.trim() } : {}),
          context_window: contextWindow,
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
          account: account.trim() || undefined,
          base_url: baseUrl.trim() || undefined,
          api_key: apiKey.trim() || undefined,
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
          <label class="add-model-label">{t('settings.accountSelect')}</label>
          <div style={{ display: 'flex', gap: '8px' }}>
            <input
              class="menu-input"
              type="text"
              placeholder="openai / deepseek / gemini"
              list="account-datalist"
              value={account}
              onInput={(e) => handleAccountChange((e.target as HTMLInputElement).value)}
            />
            <datalist id="account-datalist">
              {accounts.map((a) => (
                <option key={a.id} value={a.id}>
                  {a.id} ({a.type})
                </option>
              ))}
            </datalist>
          </div>
        </div>

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
            options={cwOptions.map((v) => ({
              value: String(v),
              label: `${fmtContextWindow(v)} tokens`,
            }))}
            onChange={(v) => setContextWindow(Number(v))}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.baseUrl')}</label>
          <input
            class="menu-input"
            type="text"
            placeholder={selectedAccount?.base_url || 'https://api.openai.com/v1'}
            value={baseUrl}
            onInput={(e) => setBaseUrl((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-field">
          <label class="add-model-label">{t('settings.apiKeyInput')}</label>
          <input
            class="menu-input"
            type="password"
            placeholder={
              isEdit
                ? t('settings.apiKeyKeep')
                : selectedAccount?.has_api_key
                  ? '（复用提供商 API Key）'
                  : 'sk-…'
            }
            value={apiKey}
            onInput={(e) => setApiKey((e.target as HTMLInputElement).value)}
          />
        </div>

        <div class="add-model-checkboxes">
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
          <div class="add-model-reasoning-fields">
            <div class="add-model-field">
              <label class="add-model-label">{t('settings.reasoningEffort')}</label>
              <Select
                value={reasoningEffort}
                options={REASONING_EFFORT_OPTIONS}
                onChange={(v) => setReasoningEffort(v)}
              />
            </div>
            <div class="add-model-field">
              <label class="add-model-label">{t('settings.reasoningHistory')}</label>
              <Select
                value={reasoningHistory}
                options={REASONING_HISTORY_OPTIONS}
                onChange={(v) => setReasoningHistory(v)}
              />
            </div>
          </div>
        )}

        {error && (
          <div class="modal-error">
            {(isEdit ? t('settings.updateFailed') : t('settings.addFailed'))}: {error}
          </div>
        )}
        <div class="modal-footer" style={{ marginTop: '16px', padding: 0 }}>
          <button class="btn" type="button" onClick={onClose} disabled={saving}>
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
