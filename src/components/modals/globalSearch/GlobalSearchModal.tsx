import { useState, useCallback, useEffect, useRef, useMemo } from "react";
import { api } from "@/services/api";
import { useTranslation } from "react-i18next";
import {
    Search,
    ArrowUp,
    ArrowDown,
    CornerDownLeft,
    X,
    Loader2,
    Filter,
    User,
    Bot,
    MessageSquare,
    Lightbulb,
} from "lucide-react";
import { Dialog, DialogContent, Input } from "@/components/ui";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Badge } from "@/components/ui/badge";
import { useAppStore } from "@/store/useAppStore";
import type { ClaudeMessage, ClaudeSession, ContentItem, SearchScopeFilter } from "@/types";
import { getProviderLabel, hasNonDefaultProvider, getProviderBadgeStyle } from "@/utils/providers";
import { cn } from "@/lib/utils";
import { toast } from "sonner";

type GlobalSearchResult = ClaudeMessage;

type MessageTypeFilter = "all" | "user" | "assistant";

type PreviewKind = "text" | "thinking" | "tool" | "toolResult" | "structured";

interface PreviewPart {
    kind: PreviewKind;
    text: string;
}

interface GlobalSearchModalProps {
    isOpen: boolean;
    onClose: () => void;
}

const SEARCH_PAGE_SIZE = 100;
const SEARCH_REQUEST_LIMIT = SEARCH_PAGE_SIZE + 1;

export const GlobalSearchModal = ({
    isOpen,
    onClose,
}: GlobalSearchModalProps) => {
    const { t } = useTranslation();
    const [query, setQuery] = useState("");
    const [results, setResults] = useState<GlobalSearchResult[]>([]);
    const [isSearching, setIsSearching] = useState(false);
    const [isLoadingMore, setIsLoadingMore] = useState(false);
    const [hasMoreResults, setHasMoreResults] = useState(false);
    const [selectedIndex, setSelectedIndex] = useState(0);
    const [messageTypeFilter, setMessageTypeFilter] = useState<MessageTypeFilter>("all");
    const [searchScope, setSearchScope] = useState<SearchScopeFilter>("text");
    const inputRef = useRef<HTMLInputElement>(null);
    const resultsContainerRef = useRef<HTMLDivElement>(null);
    const debounceTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    const { claudePath, projects, selectProject, selectSession, sessions, getSessionDisplayName, activeProviders, navigateToMessage, clearTargetMessage } =
        useAppStore();
    const [selectedProjectPath, setSelectedProjectPath] = useState<string>("all");

    // Group results by project name
    const groupedResults = useMemo(() => {
        const groups = new Map<string, { label: string; provider?: string; items: GlobalSearchResult[] }>();

        for (const result of results) {
            const projectName =
                result.projectName || t("globalSearch.unknownProject");
            const providerLabel = getProviderLabel(
                (key, fallback) => t(key, fallback),
                result.provider,
            );
            const groupKey = `${result.provider ?? "claude"}::${projectName}`;
            const groupLabel = `${projectName} (${providerLabel})`;

            if (!groups.has(groupKey)) {
                groups.set(groupKey, { label: groupLabel, provider: result.provider, items: [] });
            }
            groups.get(groupKey)!.items.push(result);
        }

        return groups;
    }, [results, t]);

    // Flatten grouped results for keyboard navigation
    const flattenedResults = useMemo(() => {
        const flat: GlobalSearchResult[] = [];
        for (const group of groupedResults.values()) {
            flat.push(...group.items);
        }
        return flat;
    }, [groupedResults]);

    // Get session display name for a search result
    const getSessionName = useCallback((result: GlobalSearchResult): string | undefined => {
        if (!result.sessionId) return undefined;
        return getSessionDisplayName(result.sessionId);
    }, [getSessionDisplayName]);

    // Debounced search
    const performSearch = useCallback(
        async (
            searchQuery: string,
            options: { append?: boolean; offset?: number } = {},
        ) => {
            const trimmedQuery = searchQuery.trim();
            const append = options.append ?? false;
            const offset = options.offset ?? 0;

            if (!claudePath || trimmedQuery.length < 2) {
                setResults([]);
                setIsSearching(false);
                setIsLoadingMore(false);
                setHasMoreResults(false);
                return;
            }

            if (append) {
                setIsLoadingMore(true);
            } else {
                setIsSearching(true);
                setHasMoreResults(false);
            }
            try {
                const filters: Record<string, unknown> = {};
                if (selectedProjectPath !== "all") {
                    // Backend matches by directory name (last path segment), not full path
                    const dirName = selectedProjectPath.split(/[\\/]/).pop() || selectedProjectPath;
                    filters.projects = [dirName];
                }
                if (messageTypeFilter !== "all") {
                    filters.messageType = messageTypeFilter;
                }
                filters.searchScope = searchScope;
                const hasNonClaudeProviders = hasNonDefaultProvider(activeProviders);
                const searchResults = await api<GlobalSearchResult[]>(
                    hasNonClaudeProviders ? "search_all_providers" : "search_messages",
                    hasNonClaudeProviders
                        ? { claudePath, query: trimmedQuery, activeProviders, filters, limit: SEARCH_REQUEST_LIMIT, offset }
                        : { claudePath, query: trimmedQuery, filters, limit: SEARCH_REQUEST_LIMIT, offset },
                );
                const visibleResults = searchResults.slice(0, SEARCH_PAGE_SIZE);
                setHasMoreResults(searchResults.length > SEARCH_PAGE_SIZE);
                setResults((previousResults) =>
                    append ? [...previousResults, ...visibleResults] : visibleResults,
                );
                if (!append) {
                    setSelectedIndex(0);
                }
            } catch (error) {
                console.error("Global search failed:", error);
                if (!append) {
                    setResults([]);
                }
                setHasMoreResults(false);
                toast.error(t("globalSearch.searchFailed"));
            } finally {
                if (append) {
                    setIsLoadingMore(false);
                } else {
                    setIsSearching(false);
                }
            }
        },
        [claudePath, activeProviders, selectedProjectPath, messageTypeFilter, searchScope, t],
    );

    const handleLoadMore = useCallback(() => {
        if (!hasMoreResults || isSearching || isLoadingMore || query.trim().length < 2) {
            return;
        }
        performSearch(query, { append: true, offset: results.length });
    }, [hasMoreResults, isSearching, isLoadingMore, query, results.length, performSearch]);

    // Handle input change with debounce
    const handleInputChange = useCallback(
        (e: React.ChangeEvent<HTMLInputElement>) => {
            const value = e.target.value;
            setQuery(value);

            if (debounceTimeoutRef.current) {
                clearTimeout(debounceTimeoutRef.current);
            }

            debounceTimeoutRef.current = setTimeout(() => {
                performSearch(value);
            }, 300);
        },
        [performSearch],
    );

    // Navigate to selected result
    const handleSelectResult = useCallback(
        async (result: GlobalSearchResult) => {
            try {
                let targetSession = sessions.find(
                    (s) =>
                        s.session_id === result.sessionId ||
                        s.actual_session_id === result.sessionId,
                );

                if (targetSession) {
                    if (result.uuid) navigateToMessage(result.uuid);
                    await selectSession(targetSession);
                    onClose();
                    return;
                }

                // Snapshot excludeSidechain once to keep requests consistent
                // across the scan and avoid repeated getState() calls. The
                // setting is user-configurable; taking a snapshot is intentional
                // so a mid-scan toggle does not change half the requests.
                const { excludeSidechain } = useAppStore.getState();
                for (const project of projects) {
                    try {
                        const projectProvider = project.provider ?? "claude";
                        const projectSessions = await api<ClaudeSession[]>(
                            projectProvider !== "claude" ? "load_provider_sessions" : "load_project_sessions",
                            projectProvider !== "claude"
                                ? { provider: projectProvider, projectPath: project.path, excludeSidechain }
                                : { projectPath: project.path, excludeSidechain },
                        );

                        targetSession = projectSessions.find(
                            (s) =>
                                s.session_id === result.sessionId ||
                                s.actual_session_id === result.sessionId,
                        );

                        if (targetSession) {
                            if (result.uuid) navigateToMessage(result.uuid);
                            await selectProject(project);
                            await selectSession(targetSession);
                            onClose();
                            return;
                        }
                    } catch (error) {
                        console.error(
                            `Failed to load sessions for project ${project.name}:`,
                            error,
                        );
                    }
                }

                // Session not found in any project
                clearTargetMessage();
                toast.error(t("globalSearch.sessionNotFound"));
                onClose();
            } catch (error) {
                clearTargetMessage();
                console.error("Failed to navigate to search result:", error);
                toast.error(t("globalSearch.navigationFailed"));
                onClose();
            }
        },
        [projects, sessions, selectProject, selectSession, navigateToMessage, clearTargetMessage, onClose, t],
    );

    // Keyboard navigation
    const handleKeyDown = useCallback(
        (e: React.KeyboardEvent) => {
            if (flattenedResults.length === 0) return;

            switch (e.key) {
                case "ArrowDown":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev < flattenedResults.length - 1 ? prev + 1 : 0,
                    );
                    break;
                case "ArrowUp":
                    e.preventDefault();
                    setSelectedIndex((prev) =>
                        prev > 0 ? prev - 1 : flattenedResults.length - 1,
                    );
                    break;
                case "Enter":
                    e.preventDefault();
                    if (flattenedResults[selectedIndex]) {
                        handleSelectResult(flattenedResults[selectedIndex]);
                    }
                    break;
                case "Escape":
                    e.preventDefault();
                    onClose();
                    break;
            }
        },
        [flattenedResults, selectedIndex, handleSelectResult, onClose],
    );

    // Scroll selected item into view
    useEffect(() => {
        if (resultsContainerRef.current && flattenedResults.length > 0) {
            const selectedElement = resultsContainerRef.current.querySelector(
                `[data-index="${selectedIndex}"]`,
            );
            selectedElement?.scrollIntoView({ block: "nearest" });
        }
    }, [selectedIndex, flattenedResults.length]);

    // Focus input when modal opens
    useEffect(() => {
        if (isOpen) {
            setTimeout(() => inputRef.current?.focus(), 0);
        } else {
            setQuery("");
            setResults([]);
            setHasMoreResults(false);
            setIsLoadingMore(false);
            setSelectedIndex(0);
            setSelectedProjectPath("all");
            setMessageTypeFilter("all");
            setSearchScope("text");
        }
    }, [isOpen]);

    // Re-search when filters change. `query` is intentionally omitted —
    // keystroke-driven searches go through handleInputChange's debounce.
    // This effect only fires when performSearch identity changes (i.e., filter deps).
    useEffect(() => {
        if (query.trim().length >= 2) {
            performSearch(query);
        }
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [performSearch]);

    // Cleanup debounce on unmount
    useEffect(() => {
        return () => {
            if (debounceTimeoutRef.current) {
                clearTimeout(debounceTimeoutRef.current);
            }
        };
    }, []);

    const stringifyPreviewValue = (value: unknown): string => {
        if (typeof value === "string") return value;
        if (value == null) return "";
        try {
            return JSON.stringify(value);
        } catch {
            return String(value);
        }
    };

    const pushPreviewPart = (parts: PreviewPart[], kind: PreviewKind, value: unknown): void => {
        const text = stringifyPreviewValue(value).trim();
        if (text) parts.push({ kind, text });
    };

    const collectPreviewParts = (content: GlobalSearchResult["content"]): PreviewPart[] => {
        const parts: PreviewPart[] = [];

        if (typeof content === "string") {
            pushPreviewPart(parts, "text", content);
            return parts;
        }

        if (!Array.isArray(content)) return parts;

        for (const item of content as ContentItem[]) {
            if (typeof item === "string") {
                pushPreviewPart(parts, "text", item);
                continue;
            }

            if (!item || typeof item !== "object") continue;

            const record = item as Record<string, unknown>;
            const itemType = typeof record.type === "string" ? record.type : "";

            if (typeof record.text === "string") {
                pushPreviewPart(parts, "text", record.text);
            }
            if (typeof record.thinking === "string") {
                pushPreviewPart(parts, "thinking", record.thinking);
            }
            if (typeof record.reasoning === "string") {
                pushPreviewPart(parts, "thinking", record.reasoning);
            }

            if (["tool_use", "server_tool_use", "mcp_tool_use"].includes(itemType)) {
                pushPreviewPart(parts, "tool", {
                    name: record.name ?? record.tool_name,
                    server: record.server_name,
                    input: record.input ?? record.arguments,
                });
            }

            if (itemType.includes("tool_result") || itemType.includes("code_execution")) {
                pushPreviewPart(parts, "toolResult", record.content ?? record);
            }

            if (itemType === "document" || itemType === "search_result" || itemType === "web_search_tool_result") {
                pushPreviewPart(parts, "structured", record.title ?? record.content ?? record.source ?? record);
            }
        }

        return parts;
    };

    const getPreviewLabel = (kind: PreviewKind): string => {
        if (kind === "thinking") return t("globalSearch.preview.thinking");
        if (kind === "tool") return t("globalSearch.preview.tool");
        if (kind === "toolResult") return t("globalSearch.preview.toolResult");
        if (kind === "structured") return t("globalSearch.preview.structured");
        return "";
    };

    const clipPreview = (text: string): string => {
        const trimmedQuery = query.trim().toLowerCase();
        if (trimmedQuery.length >= 2) {
            const lowerText = text.toLowerCase();
            const matchIndex = lowerText.indexOf(trimmedQuery);
            if (matchIndex !== -1) {
                const contextRadius = 60;
                const start = Math.max(0, matchIndex - contextRadius);
                const end = Math.min(text.length, matchIndex + trimmedQuery.length + contextRadius);
                const slice = text.slice(start, end);
                const prefix = start > 0 ? "..." : "";
                const suffix = end < text.length ? "..." : "";
                return prefix + slice + suffix;
            }
        }

        return text.slice(0, 150) + (text.length > 150 ? "..." : "");
    };

    // Get preview text centered around the search term
    const getPreviewText = (message: GlobalSearchResult): string => {
        if (!message.content) return t("globalSearch.noPreview");

        const trimmedQuery = query.trim().toLowerCase();
        const parts = collectPreviewParts(message.content);
        if (parts.length === 0) return t("globalSearch.noPreview");

        const bestPart = parts.find((part) => part.text.toLowerCase().includes(trimmedQuery)) ?? parts[0];
        if (!bestPart) return t("globalSearch.noPreview");

        const preview = clipPreview(bestPart.text);
        const label = getPreviewLabel(bestPart.kind);
        return label ? `${label}: ${preview}` : preview;
    };

    // Format timestamp
    const formatTimestamp = (timestamp: string): string => {
        try {
            const date = new Date(timestamp);
            return date.toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit",
            });
        } catch {
            return "";
        }
    };

    // Memoize regex to avoid re-creation per result item
    const highlightRegex = useMemo(() => {
        const trimmed = query.trim();
        if (!trimmed) return null;
        return new RegExp(
            `(${trimmed.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")})`,
            "i",
        );
    }, [query]);

    const highlightText = (text: string): React.ReactNode => {
        if (!highlightRegex) return text;

        const parts = text.split(highlightRegex);
        return parts.map((part, index) =>
            highlightRegex.test(part) ? (
                <mark
                    key={index}
                    className="bg-yellow-300 dark:bg-yellow-500/40 text-foreground rounded-sm px-0.5"
                >
                    {part}
                </mark>
            ) : (
                part
            ),
        );
    };

    let currentResultIndex = 0;

    return (
        <Dialog open={isOpen} onOpenChange={(open) => !open && onClose()}>
            <DialogContent
                className="sm:max-w-2xl p-0 gap-0 overflow-hidden"
                onKeyDown={handleKeyDown}
                showCloseButton={false}
                aria-label={t("globalSearch.title")}
            >
                {/* Search Header */}
                <div className="flex items-center gap-3 px-4 py-3 border-b border-border">
                    <Search className="w-4 h-4 text-muted-foreground shrink-0" />
                    <Input
                        ref={inputRef}
                        type="text"
                        value={query}
                        onChange={handleInputChange}
                        placeholder={t("globalSearch.placeholder")}
                        className="border-0 shadow-none focus-visible:ring-0 px-0 h-auto text-sm"
                        autoComplete="off"
                        autoCorrect="off"
                        autoCapitalize="off"
                        spellCheck={false}
                    />
                    {isSearching && (
                        <Loader2 className="w-4 h-4 text-muted-foreground animate-spin shrink-0" />
                    )}
                    {query && !isSearching && (
                        <button
                            onClick={() => {
                                setQuery("");
                                setResults([]);
                                setHasMoreResults(false);
                                setIsLoadingMore(false);
                                inputRef.current?.focus();
                            }}
                            className="p-1 hover:bg-muted rounded"
                            aria-label={t("globalSearch.clearSearch")}
                        >
                            <X className="w-3 h-3 text-muted-foreground" />
                        </button>
                    )}
                </div>

                {/* Filters Bar */}
                <div className="flex items-center gap-2 px-4 py-2 border-b border-border bg-muted/20">
                    {/* Message Type Filter */}
                    <div className="flex items-center gap-1">
                        {(["all", "user", "assistant"] as const).map((type) => (
                            <button
                                key={type}
                                onClick={() => setMessageTypeFilter(type)}
                                className={cn(
                                    "flex items-center gap-1 px-2 py-1 text-xs rounded-md transition-colors",
                                    messageTypeFilter === type
                                        ? "bg-foreground/10 text-foreground font-medium"
                                        : "text-muted-foreground hover:text-foreground hover:bg-muted"
                                )}
                                aria-label={t(`globalSearch.filterType.${type}`)}
                            >
                                {type === "all" && <MessageSquare className="w-3 h-3" />}
                                {type === "user" && <User className="w-3 h-3" />}
                                {type === "assistant" && <Bot className="w-3 h-3" />}
                                <span>{t(`globalSearch.filterType.${type}`)}</span>
                            </button>
                        ))}
                    </div>

                    {/* Divider */}
                    <div className="w-px h-4 bg-border" />

                    <Select value={searchScope} onValueChange={(value) => setSearchScope(value as SearchScopeFilter)}>
                        <SelectTrigger className="h-7 text-xs border-border w-36">
                            <SelectValue placeholder={t("globalSearch.scope.placeholder")} />
                        </SelectTrigger>
                        <SelectContent>
                            <SelectItem value="text">{t("globalSearch.scope.text")}</SelectItem>
                            <SelectItem value="textThinking">{t("globalSearch.scope.textThinking")}</SelectItem>
                            <SelectItem value="textTools">{t("globalSearch.scope.textTools")}</SelectItem>
                            <SelectItem value="textToolResults">{t("globalSearch.scope.textToolResults")}</SelectItem>
                            <SelectItem value="all">{t("globalSearch.scope.all")}</SelectItem>
                        </SelectContent>
                    </Select>

                    {/* Project Filter */}
                    {projects.length > 1 && (
                        <>
                            <div className="w-px h-4 bg-border" />
                            <Filter className="w-3.5 h-3.5 text-muted-foreground shrink-0" />
                            <Select value={selectedProjectPath} onValueChange={setSelectedProjectPath}>
                                <SelectTrigger className="h-7 text-xs border-border w-40">
                                    <SelectValue placeholder={t("globalSearch.allProjects")} />
                                </SelectTrigger>
                                <SelectContent>
                                    <SelectItem value="all">{t("globalSearch.allProjects")}</SelectItem>
                                    {projects.map((project) => (
                                        <SelectItem key={project.path} value={project.path}>
                                            {project.name}
                                        </SelectItem>
                                    ))}
                                </SelectContent>
                            </Select>
                        </>
                    )}
                </div>

                {/* Results */}
                <div
                    ref={resultsContainerRef}
                    className="max-h-100 overflow-y-auto"
                >
                    {/* Loading skeleton */}
                    {isSearching && results.length === 0 && (
                        <div className="py-4 space-y-3 px-4">
                            {Array.from({ length: 4 }).map((_, i) => (
                                <div key={i} className="animate-pulse">
                                    <div className="flex items-center gap-2 mb-1.5">
                                        <div className="h-4 w-12 bg-muted rounded" />
                                        <div className="h-3 w-20 bg-muted rounded" />
                                    </div>
                                    <div className="h-4 w-full bg-muted rounded mb-1" />
                                    <div className="h-4 w-3/4 bg-muted rounded" />
                                </div>
                            ))}
                        </div>
                    )}

                    {!isSearching && query.trim().length >= 2 && results.length === 0 && (
                        <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                            {t("globalSearch.noResults")}
                        </div>
                    )}

                    {/* Empty state with search tips */}
                    {!query && (
                        <div className="px-6 py-8 space-y-4">
                            <div className="text-center">
                                <Search className="w-8 h-8 text-muted-foreground/40 mx-auto mb-3" />
                                <p className="text-sm text-muted-foreground">
                                    {t("globalSearch.hint")}
                                </p>
                            </div>
                            <div className="space-y-2">
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.minChars")}</span>
                                </div>
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.filters")}</span>
                                </div>
                                <div className="flex items-start gap-2 text-xs text-muted-foreground/70">
                                    <Lightbulb className="w-3.5 h-3.5 mt-0.5 shrink-0" />
                                    <span>{t("globalSearch.tips.navigate")}</span>
                                </div>
                            </div>
                        </div>
                    )}

                    {/* Typing but not enough chars */}
                    {query && query.trim().length < 2 && !isSearching && (
                        <div className="px-4 py-8 text-center text-sm text-muted-foreground">
                            {t("globalSearch.tips.minChars")}
                        </div>
                    )}

                    {results.length > 0 && (
                        <div className="py-2">
                            {Array.from(groupedResults.entries()).map(
                                ([groupKey, group]) => (
                                    <div key={groupKey}>
                                        {/* Project Header */}
                                        <div className="px-4 py-1.5 text-xs font-medium text-muted-foreground bg-muted sticky top-0 truncate flex items-center gap-2">
                                            {group.provider && group.provider !== "claude" && (
                                                <Badge
                                                    size="sm"
                                                    className={cn(
                                                        "rounded px-1 py-0 text-2xs",
                                                        getProviderBadgeStyle(group.provider)
                                                    )}
                                                >
                                                    {getProviderLabel((key, fallback) => t(key, fallback), group.provider)}
                                                </Badge>
                                            )}
                                            <span className="truncate">{group.label}</span>
                                        </div>

                                        {/* Results in this project */}
                                        {group.items.map((result) => {
                                            const index = currentResultIndex++;
                                            const isSelected = index === selectedIndex;

                                            return (
                                                <button
                                                    key={result.uuid}
                                                    data-index={index}
                                                    onClick={() => handleSelectResult(result)}
                                                    className={cn(
                                                        "w-full text-left px-4 py-2.5 hover:bg-muted/50 transition-colors",
                                                        isSelected && "bg-muted"
                                                    )}
                                                >
                                                    <div className="flex items-start gap-3">
                                                        <div className="flex-1 min-w-0">
                                                            <div className="flex items-center gap-2 mb-1">
                                                                <span
                                                                    className={cn(
                                                                        "inline-flex items-center gap-1 text-xs px-1.5 py-0.5 rounded font-medium",
                                                                        result.type === "user"
                                                                            ? "bg-blue-500/10 text-blue-500"
                                                                            : result.type === "assistant"
                                                                              ? "bg-amber-500/10 text-amber-500"
                                                                              : "bg-gray-500/10 text-gray-500"
                                                                    )}
                                                                >
                                                                    {result.type === "user" && <User className="w-3 h-3" />}
                                                                    {result.type === "assistant" && <Bot className="w-3 h-3" />}
                                                                    {result.type}
                                                                </span>
                                                                <span className="text-xs text-muted-foreground">
                                                                    {formatTimestamp(result.timestamp)}
                                                                </span>
                                                            </div>
                                                            {(() => {
                                                                const sessionName = getSessionName(result);
                                                                return sessionName ? (
                                                                    <p className="text-xs text-muted-foreground/70 truncate mb-0.5">
                                                                        {sessionName}
                                                                    </p>
                                                                ) : null;
                                                            })()}
                                                            <p className="text-sm text-foreground line-clamp-2">
                                                                {highlightText(getPreviewText(result))}
                                                            </p>
                                                        </div>
                                                    </div>
                                                </button>
                                            );
                                        })}
                                    </div>
                                ),
                            )}
                            {hasMoreResults && (
                                <div className="px-4 pt-2 pb-1">
                                    <button
                                        type="button"
                                        onClick={handleLoadMore}
                                        disabled={isLoadingMore}
                                        className="w-full h-8 inline-flex items-center justify-center gap-2 rounded-md border border-border bg-background text-xs font-medium text-muted-foreground hover:text-foreground hover:bg-muted disabled:opacity-60 disabled:cursor-not-allowed"
                                    >
                                        {isLoadingMore && (
                                            <Loader2 className="w-3.5 h-3.5 animate-spin" />
                                        )}
                                        <span>
                                            {isLoadingMore
                                                ? t("globalSearch.loadingMore")
                                                : t("globalSearch.loadMore", { count: SEARCH_PAGE_SIZE })}
                                        </span>
                                    </button>
                                </div>
                            )}
                        </div>
                    )}
                </div>

                {/* Footer with keyboard hints */}
                <div className="flex items-center justify-between px-4 py-2 border-t border-border bg-muted/30 text-xs text-muted-foreground">
                    <div className="flex items-center gap-4">
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <ArrowUp className="w-3 h-3 inline" />
                            </kbd>
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <ArrowDown className="w-3 h-3 inline" />
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.navigate")}
                            </span>
                        </div>
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono">
                                <CornerDownLeft className="w-3 h-3 inline" />
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.select")}
                            </span>
                        </div>
                        <div className="flex items-center gap-1">
                            <kbd className="px-1.5 py-0.5 bg-muted rounded border border-border font-mono text-[10px]">
                                esc
                            </kbd>
                            <span className="ml-1">
                                {t("globalSearch.close")}
                            </span>
                        </div>
                    </div>
                    {results.length > 0 && (
                        <span>
                            {t("globalSearch.results", {
                                count: results.length,
                            })}
                        </span>
                    )}
                </div>
            </DialogContent>
        </Dialog>
    );
};
