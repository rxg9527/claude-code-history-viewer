import type { StateCreator } from "zustand";
import type { FullAppStore } from "./types";
import type { DateFilter } from "../../types/board.types";
import { storageAdapter } from "@/services/storage";

export interface MessageFilterRoles {
    user: boolean;
    assistant: boolean;
}

export interface MessageFilterContentTypes {
    text: boolean;
    thinking: boolean;
    toolCalls: boolean;
    commands: boolean;
}

export interface MessageFilter {
    roles: MessageFilterRoles;
    contentTypes: MessageFilterContentTypes;
}

const DEFAULT_MESSAGE_FILTER: MessageFilter = {
    roles: { user: true, assistant: true },
    contentTypes: { text: true, thinking: true, toolCalls: true, commands: true },
};

const MESSAGE_FILTER_STORAGE_KEY = "messageFilter";

const normalizeMessageFilter = (value: unknown): MessageFilter => {
    const stored = value as Partial<MessageFilter> | null | undefined;
    return {
        roles: {
            ...DEFAULT_MESSAGE_FILTER.roles,
            ...(stored?.roles ?? {}),
        },
        contentTypes: {
            ...DEFAULT_MESSAGE_FILTER.contentTypes,
            ...(stored?.contentTypes ?? {}),
        },
    };
};

const persistMessageFilter = async (messageFilter: MessageFilter) => {
    try {
        const store = await storageAdapter.load("settings.json", {
            autoSave: false,
            defaults: {},
        });
        await store.set(MESSAGE_FILTER_STORAGE_KEY, messageFilter);
        await store.save();
    } catch (error) {
        console.warn("Failed to save message filter:", error);
    }
};

export interface FilterSliceState {
    dateFilter: DateFilter;
    userOnlyFilter: boolean;
    messageFilter: MessageFilter;
}

export interface FilterSliceActions {
    setDateFilter: (filter: DateFilter) => void;
    clearDateFilter: () => void;
    setUserOnlyFilter: (enabled: boolean) => void;
    toggleUserOnlyFilter: () => void;
    toggleRole: (role: keyof MessageFilterRoles) => void;
    toggleContentType: (contentType: keyof MessageFilterContentTypes) => void;
    resetMessageFilter: () => void;
    loadMessageFilter: () => Promise<void>;
    isMessageFilterActive: () => boolean;
}

export type FilterSlice = FilterSliceState & FilterSliceActions;

const getInitialDateFilter = () => ({ start: null, end: null });

const initialFilterState: FilterSliceState = {
    dateFilter: getInitialDateFilter(),
    userOnlyFilter: false,
    messageFilter: { ...DEFAULT_MESSAGE_FILTER },
};

export const createFilterSlice: StateCreator<
    FullAppStore,
    [],
    [],
    FilterSlice
> = (set, get) => ({
    ...initialFilterState,

    setDateFilter: (dateFilter) => {
        set({ dateFilter });
    },

    clearDateFilter: () => {
        set({ dateFilter: { start: null, end: null } });
    },

    setUserOnlyFilter: (enabled) => {
        set({ userOnlyFilter: enabled });
    },

    toggleUserOnlyFilter: () => {
        set((state) => ({ userOnlyFilter: !state.userOnlyFilter }));
    },

    toggleRole: (role) => {
        set((state) => ({
            messageFilter: {
                ...state.messageFilter,
                roles: {
                    ...state.messageFilter.roles,
                    [role]: !state.messageFilter.roles[role],
                },
            },
        }));
        void persistMessageFilter(get().messageFilter);
    },

    toggleContentType: (contentType) => {
        set((state) => ({
            messageFilter: {
                ...state.messageFilter,
                contentTypes: {
                    ...state.messageFilter.contentTypes,
                    [contentType]: !state.messageFilter.contentTypes[contentType],
                },
            },
        }));
        void persistMessageFilter(get().messageFilter);
    },

    resetMessageFilter: () => {
        const messageFilter = {
            roles: { ...DEFAULT_MESSAGE_FILTER.roles },
            contentTypes: { ...DEFAULT_MESSAGE_FILTER.contentTypes },
        };
        set({ messageFilter });
        void persistMessageFilter(messageFilter);
    },

    loadMessageFilter: async () => {
        try {
            const store = await storageAdapter.load("settings.json", {
                autoSave: false,
                defaults: {},
            });
            const savedFilter = await store.get<MessageFilter>(MESSAGE_FILTER_STORAGE_KEY);
            if (savedFilter) {
                set({ messageFilter: normalizeMessageFilter(savedFilter) });
            }
        } catch (error) {
            console.warn("Failed to load persisted message filter:", error);
        }
    },

    isMessageFilterActive: () => {
        const { messageFilter } = get();
        const { roles, contentTypes } = messageFilter;
        return !roles.user || !roles.assistant
            || !contentTypes.text || !contentTypes.thinking
            || !contentTypes.toolCalls || !contentTypes.commands;
    },
});
