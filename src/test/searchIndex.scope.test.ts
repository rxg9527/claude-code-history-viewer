import { beforeEach, describe, expect, it } from "vitest";
import type { ClaudeMessage } from "../types";
import {
  buildSearchIndex,
  clearSearchIndex,
  searchMessages,
} from "../utils/searchIndex";

const baseMessage = {
  sessionId: "session-1",
  timestamp: "2026-06-14T00:00:00Z",
} satisfies Pick<ClaudeMessage, "sessionId" | "timestamp">;

describe("searchIndex search scopes", () => {
  beforeEach(() => {
    clearSearchIndex();
  });

  it("keeps text scope from matching tool results", () => {
    const messages: ClaudeMessage[] = [
      {
        ...baseMessage,
        uuid: "text-message",
        type: "assistant",
        role: "assistant",
        content: [{ type: "text", text: "plain response" }],
      },
      {
        ...baseMessage,
        uuid: "tool-result-message",
        type: "user",
        role: "user",
        content: [
          {
            type: "tool_result",
            tool_use_id: "tool-1",
            content: "needle from command output",
          },
        ],
      },
    ];

    buildSearchIndex(messages);

    expect(searchMessages("needle", "content", "text")).toEqual([]);
    expect(searchMessages("needle", "content", "textToolResults")).toEqual([
      {
        messageUuid: "tool-result-message",
        messageIndex: 1,
        matchIndex: 0,
        matchCount: 1,
      },
    ]);
  });
});
