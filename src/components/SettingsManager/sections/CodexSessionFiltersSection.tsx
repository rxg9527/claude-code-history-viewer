import { useTranslation } from "react-i18next";
import {
  Collapsible,
  CollapsibleContent,
  CollapsibleTrigger,
} from "@/components/ui/collapsible";
import { Switch } from "@/components/ui/switch";
import { Label } from "@/components/ui/label";
import { ChevronDown, ChevronRight, ShieldCheck } from "lucide-react";
import { useAppStore } from "@/store/useAppStore";
import { normalizeCodexSessionFilters } from "@/lib/codexSessionFilters";
import type { CodexSessionFilterSettings } from "@/types";

interface CodexSessionFiltersSectionProps {
  isExpanded: boolean;
  onToggle: (open: boolean) => void;
}

export function CodexSessionFiltersSection({
  isExpanded,
  onToggle,
}: CodexSessionFiltersSectionProps) {
  const { t } = useTranslation();
  const {
    userMetadata,
    updateUserSettings,
    invalidateProjectSessionsCache,
    scanProjects,
    selectedProject,
    selectProject,
  } = useAppStore();

  const filters = normalizeCodexSessionFilters(
    userMetadata?.settings?.codexSessionFilters
  );

  const updateFilters = async (updates: Partial<CodexSessionFilterSettings>) => {
    const nextFilters = normalizeCodexSessionFilters({ ...filters, ...updates });
    await updateUserSettings({ codexSessionFilters: nextFilters });
    invalidateProjectSessionsCache(undefined, "codex");
    await scanProjects();
    if (selectedProject?.provider === "codex") {
      await selectProject(selectedProject, { forceRefresh: true });
    }
  };

  return (
    <Collapsible open={isExpanded} onOpenChange={onToggle}>
      <CollapsibleTrigger className="flex w-full items-center gap-2 rounded-lg px-3 py-2.5 text-sm font-medium hover:bg-muted/50 transition-colors">
        {isExpanded ? (
          <ChevronDown className="h-4 w-4 shrink-0" />
        ) : (
          <ChevronRight className="h-4 w-4 shrink-0" />
        )}
        <ShieldCheck className="h-4 w-4 shrink-0 text-muted-foreground" />
        <span>{t("settings.codexFilters.title")}</span>
      </CollapsibleTrigger>

      <CollapsibleContent>
        <div className="space-y-3 px-3 pb-3">
          <p className="text-xs text-muted-foreground">
            {t("settings.codexFilters.description")}
          </p>

          <div className="flex items-center justify-between gap-3">
            <Label
              htmlFor="codex-filters-enabled"
              className="text-sm cursor-pointer"
            >
              {t("settings.codexFilters.enable")}
            </Label>
            <Switch
              id="codex-filters-enabled"
              checked={filters.enabled}
              onCheckedChange={(checked) => updateFilters({ enabled: checked })}
            />
          </div>

          {filters.enabled && (
            <div className="space-y-2 rounded-md border border-border/60 bg-muted/20 px-3 py-2">
              <div className="flex items-center justify-between gap-3">
                <div className="min-w-0">
                  <Label
                    htmlFor="codex-include-permissions"
                    className="text-sm cursor-pointer"
                  >
                    {t("settings.codexFilters.permissions")}
                  </Label>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {t("settings.codexFilters.permissionsDescription")}
                  </p>
                </div>
                <Switch
                  id="codex-include-permissions"
                  checked={filters.includePermissions}
                  onCheckedChange={(checked) =>
                    updateFilters({ includePermissions: checked })
                  }
                />
              </div>
            </div>
          )}
        </div>
      </CollapsibleContent>
    </Collapsible>
  );
}
