import { beforeEach, describe, expect, it, vi } from "vitest";
import { useAppStore } from "@/store/useAppStore";
import { api } from "@/services/api";
import type { ClaudeProject, ClaudeSession } from "@/types";

vi.mock("@/services/api", () => ({
  api: vi.fn(),
}));

const createProject = (overrides: Partial<ClaudeProject> = {}): ClaudeProject => {
  const path = overrides.path ?? "/projects/app";
  return {
    name: overrides.name ?? "app",
    path,
    actual_path: overrides.actual_path ?? path,
    session_count: overrides.session_count ?? 1,
    message_count: overrides.message_count ?? 10,
    last_modified: overrides.last_modified ?? "2026-06-14T00:00:00Z",
    provider: overrides.provider ?? "claude",
    ...overrides,
  };
};

const createSession = (overrides: Partial<ClaudeSession> = {}): ClaudeSession => ({
  session_id: overrides.session_id ?? "/projects/app/session.jsonl",
  actual_session_id: overrides.actual_session_id ?? "session",
  file_path: overrides.file_path ?? "/projects/app/session.jsonl",
  project_name: overrides.project_name ?? "app",
  message_count: overrides.message_count ?? 1,
  first_message_time: overrides.first_message_time ?? "2026-06-14T00:00:00Z",
  last_message_time: overrides.last_message_time ?? "2026-06-14T00:00:00Z",
  last_modified: overrides.last_modified ?? "2026-06-14T00:00:00Z",
  has_tool_use: overrides.has_tool_use ?? false,
  has_errors: overrides.has_errors ?? false,
  provider: overrides.provider ?? "claude",
  ...overrides,
});

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
};

const createDeferred = <T,>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((res) => {
    resolve = res;
  });
  return { promise, resolve };
};

describe("projectSlice selectProject session cache", () => {
  const mockApi = vi.mocked(api);

  beforeEach(() => {
    vi.clearAllMocks();
    useAppStore.setState({
      projects: [],
      selectedProject: null,
      sessions: [],
      selectedSession: null,
      projectSessionsCache: {},
      excludeSidechain: false,
      isLoadingSessions: false,
      error: null,
    });
  });

  it("reuses cached sessions when the same project signature is selected again", async () => {
    const project = createProject();
    const sessions = [createSession()];
    mockApi.mockResolvedValueOnce(sessions);

    await useAppStore.getState().selectProject(project);
    useAppStore.getState().clearProjectSelection();
    await useAppStore.getState().selectProject(project);

    expect(mockApi).toHaveBeenCalledTimes(1);
    expect(useAppStore.getState().sessions).toEqual(sessions);
    expect(useAppStore.getState().isLoadingSessions).toBe(false);
  });

  it("loads again when the project signature changes", async () => {
    const project = createProject();
    const changedProject = createProject({ session_count: 2 });
    const firstSessions = [createSession({ session_id: "first" })];
    const secondSessions = [
      createSession({ session_id: "second-a" }),
      createSession({ session_id: "second-b" }),
    ];
    mockApi.mockResolvedValueOnce(firstSessions).mockResolvedValueOnce(secondSessions);

    await useAppStore.getState().selectProject(project);
    await useAppStore.getState().selectProject(changedProject);

    expect(mockApi).toHaveBeenCalledTimes(2);
    expect(useAppStore.getState().sessions).toEqual(secondSessions);
  });

  it("keeps separate cache entries for excludeSidechain changes", async () => {
    const project = createProject();
    const firstSessions = [createSession({ session_id: "with-sidechain" })];
    const secondSessions = [createSession({ session_id: "without-sidechain" })];
    mockApi.mockResolvedValueOnce(firstSessions).mockResolvedValueOnce(secondSessions);

    await useAppStore.getState().selectProject(project);
    useAppStore.setState({ excludeSidechain: true });
    await useAppStore.getState().selectProject(project);

    expect(mockApi).toHaveBeenCalledTimes(2);
    expect(useAppStore.getState().sessions).toEqual(secondSessions);
  });

  it("does not let an older project request overwrite the latest selection", async () => {
    const first = createDeferred<ClaudeSession[]>();
    const second = createDeferred<ClaudeSession[]>();
    const projectA = createProject({ name: "a", path: "/projects/a", actual_path: "/projects/a" });
    const projectB = createProject({ name: "b", path: "/projects/b", actual_path: "/projects/b" });
    const sessionsA = [createSession({ session_id: "a", project_name: "a" })];
    const sessionsB = [createSession({ session_id: "b", project_name: "b" })];
    mockApi.mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise);

    const firstLoad = useAppStore.getState().selectProject(projectA);
    const secondLoad = useAppStore.getState().selectProject(projectB);

    second.resolve(sessionsB);
    await secondLoad;
    first.resolve(sessionsA);
    await firstLoad;

    expect(useAppStore.getState().selectedProject?.path).toBe(projectB.path);
    expect(useAppStore.getState().sessions).toEqual(sessionsB);
    expect(useAppStore.getState().isLoadingSessions).toBe(false);
  });
});
