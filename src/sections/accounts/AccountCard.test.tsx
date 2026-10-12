import { describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { AccountSummary, AppStatus } from "../../types";
import { AccountCard } from "./AccountCard";

function mkAccount(overrides: Partial<AccountSummary> = {}): AccountSummary {
  return {
    uuid: "acct-1",
    email: "a@example.com",
    org_name: "personal",
    subscription_type: null,
    is_cli_active: false,
    is_desktop_active: false,
    has_cli_credentials: true,
    credentials_healthy: true,
    has_desktop_profile: false,
    desktop_profile_on_disk: false,
    verify_status: "never",
    verified_email: null,
    verified_at: null,
    drift: false,
    token_status: "valid",
    token_remaining_mins: null,
    login_expires_at: null,
    last_cli_switch: null,
    last_desktop_switch: null,
    ...overrides,
  };
}

function mkStatus(overrides: Partial<AppStatus> = {}): AppStatus {
  return {
    platform: "macos",
    arch: "arm64",
    cli_active_email: null,
    desktop_active_email: null,
    desktop_installed: true,
    account_count: 1,
    data_dir: "/tmp/claudepot",
    cc_config_dir: "/tmp/.claude",
    dev_build: false,
    ...overrides,
  };
}

const noopHandlers = {
  cliHandlers: {
    switchCli: () => {},
    verify: () => {},
    login: () => {},
  },
  desktopHandlers: {
    switchDesktop: () => {},
    switchDesktopNoLaunch: () => {},
    launchDesktop: () => {},
    adoptDesktop: () => {},
  },
};

describe("AccountCard — tokens chip (M-6)", () => {
  it("does not render the chip when token count is zero", () => {
    render(
      <AccountCard
        account={mkAccount()}
        usageEntry={null}
        status={mkStatus()}
        onLogin={() => {}}
        {...noopHandlers}
      />,
    );
    expect(
      screen.queryByRole("button", { name: /open keys filtered to/i }),
    ).toBeNull();
  });

  it("renders 'N tokens' and fires the navigate + filter flow on click", async () => {
    const onOpenTokens = vi.fn();
    render(
      <AccountCard
        account={mkAccount()}
        usageEntry={null}
        status={mkStatus()}
        onLogin={() => {}}
        tokenCount={3}
        onOpenTokens={onOpenTokens}
        {...noopHandlers}
      />,
    );
    const chip = screen.getByRole("button", {
      name: /open keys filtered to a@example\.com/i,
    });
    expect(chip).toHaveTextContent("3 tokens");
    await userEvent.click(chip);
    expect(onOpenTokens).toHaveBeenCalledWith("a@example.com");
  });

  it("singularizes the count when exactly one token is stored", () => {
    render(
      <AccountCard
        account={mkAccount()}
        usageEntry={null}
        status={mkStatus()}
        onLogin={() => {}}
        tokenCount={1}
        onOpenTokens={() => {}}
        {...noopHandlers}
      />,
    );
    expect(
      screen.getByRole("button", { name: /open keys filtered to/i }),
    ).toHaveTextContent("1 token");
  });
});

describe("AccountCard — login end date", () => {
  const renderWith = (login_expires_at: string | null) =>
    render(
      <AccountCard
        account={mkAccount({ login_expires_at })}
        usageEntry={null}
        status={mkStatus()}
        onLogin={() => {}}
        {...noopHandlers}
      />,
    );
  const inDays = (n: number) => new Date(Date.now() + n * 86_400_000).toISOString();

  it("renders nothing when the end date is unknown", () => {
    renderWith(null);
    expect(screen.queryByText(/login end/i)).toBeNull();
  });

  it("says when the login ends, with the full date on hover", () => {
    renderWith(inDays(10));
    const cell = screen.getByText(/login ends in \d+d/i);
    expect(cell.getAttribute("title")).toBeTruthy();
  });

  it("says the login has ended once the date is past", () => {
    renderWith(inDays(-1));
    expect(screen.getByText(/login ended/i)).toBeInTheDocument();
  });
});
