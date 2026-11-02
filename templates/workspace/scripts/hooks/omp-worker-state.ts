// QUAQUA_EXTENSION_ID=omp-worker-state
// QUAQUA_EXTENSION_VERSION=1
// TARGETED_OMP_VERSION=18.15.0

import fs from "node:fs";
import path from "node:path";

export const TARGETED_OMP_VERSION = "18.15.0";
export const EXTENSION_VERSION = 1;

export interface AssistantMessage {
  role?: string;
  stopReason?: string;
  errorMessage?: string;
  errorStatus?: number;
  errorId?: number;
}

export interface AgentEndEvent {
  willContinue?: boolean;
  messages?: unknown[];
}

export interface ToolEvent {
  toolName?: string;
  args?: unknown;
}

export interface SessionContext {
  isIdle?: () => boolean;
  hasUI?: boolean;
}

export interface OmpExtensionApi {
  on(event: string, handler: (...args: unknown[]) => void): void;
}

export function isNestedOmpSession(): boolean {
  return process.env.OMPCODE === "1";
}

export function writeAtomic(targetPath: string, content: string): void {
  const dir = path.dirname(targetPath);
  const tmpPath = `${targetPath}.${process.pid}.${Date.now()}.${Math.random().toString(36).slice(2)}.tmp`;
  try {
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(tmpPath, content, "utf8");
    fs.renameSync(tmpPath, targetPath);
  } catch (err) {
    try {
      if (fs.existsSync(tmpPath)) {
        fs.unlinkSync(tmpPath);
      }
    } catch {}
    throw err;
  }
}

export function classifyError(
  status: number,
  errorMessage: string
): { errorClass: "fatal" | "retryable"; status: number } {
  // Connection refused is always retryable with status 0
  if (
    status === 0 ||
    /connection.?refused|econnrefused|couldn.?t connect/i.test(errorMessage)
  ) {
    return { errorClass: "retryable", status: 0 };
  }

  // 401 and 403 (invalid api key, quota, auth) are fatal
  if (status === 401 || status === 403) {
    return { errorClass: "fatal", status };
  }

  // 408, 409, 425, 429, 5xx, 529 are retryable
  if (
    status === 408 ||
    status === 409 ||
    status === 425 ||
    status === 429 ||
    status === 529 ||
    (status >= 500 && status <= 599)
  ) {
    return { errorClass: "retryable", status };
  }

  // Generic network errors are retryable
  if (
    /econnreset|socket hang up|network.?error|fetch failed|timed? out|timeout|overloaded/i.test(
      errorMessage
    )
  ) {
    return { errorClass: "retryable", status };
  }

  // Other 4xx client errors are fatal
  if (status >= 400 && status < 500) {
    return { errorClass: "fatal", status };
  }

  return { errorClass: "fatal", status };
}

export function findLastAssistantMessage(messages: unknown): AssistantMessage | undefined {
  if (!Array.isArray(messages)) {
    return undefined;
  }
  for (let i = messages.length - 1; i >= 0; i--) {
    const msg = messages[i];
    if (msg && typeof msg === "object" && (msg as AssistantMessage).role === "assistant") {
      return msg as AssistantMessage;
    }
  }
  return undefined;
}

export function extractErrorDetails(event: unknown): {
  errorClass: "fatal" | "retryable";
  status: number;
  attempts: number;
  isAuth: boolean;
} | undefined {
  const endEvent = (event && typeof event === "object" ? event : {}) as AgentEndEvent;
  const assistant = findLastAssistantMessage(endEvent.messages);
  if (!assistant || assistant.stopReason !== "error") {
    return undefined;
  }

  const errorMessage = String(assistant.errorMessage ?? "");
  let rawStatus = 0;
  if (typeof assistant.errorStatus === "number" && assistant.errorStatus > 0) {
    rawStatus = assistant.errorStatus;
  } else if (
    typeof assistant.errorId === "number" &&
    assistant.errorId >= 100 &&
    assistant.errorId <= 599
  ) {
    rawStatus = assistant.errorId;
  } else {
    const m = errorMessage.match(/\b([1-5]\d\d)\b/);
    if (m) {
      rawStatus = parseInt(m[1], 10);
    } else {
      rawStatus = 0;
    }
  }

  const { errorClass, status } = classifyError(rawStatus, errorMessage);

  let attempts = 1;
  const retryMatch = errorMessage.match(
    /(?:retries|attempts):\s*|after\s+(\d+)\s+(?:retries|attempts)/i
  );
  if (retryMatch && retryMatch[1]) {
    attempts = parseInt(retryMatch[1], 10);
  }

  const isAuth =
    status === 401 ||
    /invalid.?api.?key|not logged in|log in|sign in|signed out|auth_required|invalid credential/i.test(
      errorMessage
    );

  return { errorClass, status, attempts, isAuth };
}

export default function (pi: OmpExtensionApi) {
  if (isNestedOmpSession()) {
    return;
  }

  const stateFile = process.env.QUAQUA_STATE_FILE;
  const errorFile = process.env.QUAQUA_ERROR_FILE;

  let agentActive = false;
  let sessionAttempts = 0;

  function writeState(state: string, reason?: string): void {
    if (!stateFile) {
      return;
    }
    const epoch = Math.floor(Date.now() / 1000);
    const line = reason ? `${state} ${epoch} ${reason}\n` : `${state} ${epoch}\n`;
    try {
      writeAtomic(stateFile, line);
    } catch {}
  }

  function writeError(
    errorClass: "fatal" | "retryable",
    status: number,
    attempts: number
  ): void {
    if (!errorFile) {
      return;
    }
    const line = `${errorClass} ${status} ${attempts}\n`;
    try {
      writeAtomic(errorFile, line);
    } catch {}
  }

  pi.on("session_start", (_event: unknown, ctx: unknown) => {
    const sessionCtx = ctx as SessionContext | undefined;
    agentActive = sessionCtx?.isIdle?.() === false;
    if (agentActive) {
      writeState("working");
    } else {
      writeState("idle");
    }
  });

  pi.on("agent_start", () => {
    agentActive = true;
    sessionAttempts++;
    writeState("working");
  });

  pi.on("tool_approval_requested", () => {
    writeState("blocked", "permission");
  });

  pi.on("tool_approval_resolved", () => {
    if (agentActive) {
      writeState("working");
    }
  });

  pi.on("plan_review_requested", () => {
    writeState("blocked", "plan");
  });

  pi.on("plan_review_resolved", () => {
    if (agentActive) {
      writeState("working");
    }
  });

  pi.on("tool_execution_start", (event: unknown) => {
    const toolEv = event as ToolEvent | undefined;
    if (toolEv?.toolName === "ask") {
      writeState("blocked", "question");
    }
  });

  pi.on("tool_execution_end", (event: unknown) => {
    const toolEv = event as ToolEvent | undefined;
    if (toolEv?.toolName === "ask") {
      if (agentActive) {
        writeState("working");
      }
    }
  });

  pi.on("credential_disabled", () => {
    writeState("blocked", "signed-out");
    writeError("fatal", 401, Math.max(1, sessionAttempts));
  });

  pi.on("agent_end", (event: unknown) => {
    const endEv = event as AgentEndEvent | undefined;
    if (endEv?.willContinue === true) {
      // Continuation scheduled; stay working
      return;
    }

    agentActive = false;

    const errorDetails = extractErrorDetails(event);
    if (errorDetails) {
      const finalAttempts = Math.max(sessionAttempts, errorDetails.attempts);
      writeError(errorDetails.errorClass, errorDetails.status, finalAttempts);
      sessionAttempts = 0;

      if (errorDetails.isAuth) {
        writeState("blocked", "signed-out");
      } else {
        writeState("idle");
      }
    } else {
      sessionAttempts = 0;
      writeState("idle");
    }
  });
}
