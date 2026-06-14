import type { CodexSessionFilterSettings, UserSettings } from "@/types";

export const DEFAULT_CODEX_SESSION_FILTERS: CodexSessionFilterSettings = {
  enabled: true,
  includePermissions: false,
  includeGitCommitSubagents: false,
};

export const normalizeCodexSessionFilters = (
  filters?: Partial<CodexSessionFilterSettings>
): CodexSessionFilterSettings => ({
  enabled: filters?.enabled ?? DEFAULT_CODEX_SESSION_FILTERS.enabled,
  includePermissions:
    filters?.includePermissions ?? DEFAULT_CODEX_SESSION_FILTERS.includePermissions,
  includeGitCommitSubagents:
    filters?.includeGitCommitSubagents ??
    DEFAULT_CODEX_SESSION_FILTERS.includeGitCommitSubagents,
});

export const getCodexSessionFiltersParam = (
  settings?: UserSettings
): CodexSessionFilterSettings =>
  normalizeCodexSessionFilters(settings?.codexSessionFilters);
