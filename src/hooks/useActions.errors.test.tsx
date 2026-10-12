import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, renderHook } from "@testing-library/react";
import { i18n } from "../lib/i18n";
import { sampleAccount } from "../test/fixtures";
import { useActions } from "./useActions";

/**
 * Errors are classified by code, never by the rendered text.
 *
 * The rendered text is translated, so every check that looked for an
 * English phrase in it ("claude code process is running", "already in
 * progress") stopped firing in the zh-CN UI: the force-switch offer
 * never appeared, and a login already in progress read as a failure.
 * These run in zh-CN on purpose — in English the old checks passed.
 */

const cliUse = vi.fn();
const accountLoginStart = vi.fn();
vi.mock("../api", () => ({
  api: {
    cliUse: (...a: unknown[]) => cliUse(...a),
    accountLoginStart: (...a: unknown[]) => accountLoginStart(...a),
  },
}));
vi.mock("@tauri-apps/api/event", () => ({ emit: vi.fn() }));

const dto = (code: string, message: string) => ({ code, message });

function setup() {
  const pushToast = vi.fn();
  const { result } = renderHook(() =>
    useActions({
      pushToast,
      refresh: async () => {},
      withBusy: (_key, fn) => fn(),
      openOpModal: () => {},
    }),
  );
  return { pushToast, actions: result.current };
}

beforeEach(async () => {
  vi.clearAllMocks();
  await i18n.changeLanguage("zh-CN");
});
afterEach(async () => {
  await i18n.changeLanguage("en");
});

describe("useActions error classification (zh-CN)", () => {
  it("offers a forced retry when an older Claude Code blocks the switch", async () => {
    cliUse.mockRejectedValue(
      dto("cli_backend.live_session_conflict", "a Claude Code process older than 2.1.296 is running"),
    );
    const { pushToast, actions } = setup();
    await act(() => actions.useCli(sampleAccount()));

    const [kind, , onUndo] = pushToast.mock.calls[0];
    expect(kind).toBe("error");
    expect(typeof onUndo).toBe("function");
  });

  it("reports a login already in progress as that, not as a failure", async () => {
    accountLoginStart.mockRejectedValue(
      dto("accounts.login_in_progress", "a login is already in progress"),
    );
    const { pushToast, actions } = setup();
    await act(() => actions.login(sampleAccount()));

    expect(pushToast).toHaveBeenCalledWith("error", i18n.t("account.loginInProgress"));
  });

  it("reports a cancelled login as information", async () => {
    accountLoginStart.mockRejectedValue(
      dto("onboard.auth_login_cancelled", "login cancelled"),
    );
    const { pushToast, actions } = setup();
    await act(() => actions.login(sampleAccount()));

    expect(pushToast).toHaveBeenCalledWith("info", i18n.t("account.loginCancelled"));
  });
});
