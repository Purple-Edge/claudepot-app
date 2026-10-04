import { useEffect, useState, type ReactNode } from "react";
import { Trans, useTranslation } from "react-i18next";
import {
  GRANT_DURATION_PRESETS,
  permissionModeLabel,
  type ProjectPermission,
} from "../../api/permission";
import { Button } from "../../components/primitives/Button";
import { Tag } from "../../components/primitives/Tag";
import { NF } from "../../icons";
import { i18n } from "../../lib/i18n";
import { useProjectPermission } from "./useProjectPermission";

/** "1h 47m", "47m", or "<1m" for a positive millisecond span. */
function formatRemaining(ms: number): string {
  const t = i18n.getFixedT(null, "projects");
  if (ms <= 0) return t("permission.expired");
  const totalMin = Math.floor(ms / 60_000);
  if (totalMin < 1) return t("permission.underMinute");
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  return h > 0
    ? t("permission.hoursMinutes", { h, m })
    : t("permission.minutesOnly", { m });
}

/**
 * Per-project permission control. A grant makes Claudepot answer
 * Claude Code's `PreToolUse` check with `allow` for tool calls whose
 * session is inside this project, until the grant lapses. It writes
 * nothing into the project's settings: Claude Code has ignored
 * `bypassPermissions` from project files since 2.1.257, and a value
 * left there by an older Claudepot is rendered here as *ignored*, with
 * a one-click removal.
 *
 * The dangerous part of unattended approval isn't switching it on —
 * it's forgetting to switch it off. Time-boxed grants lapse on their
 * own and a live countdown keeps the state visible; the sticky preset
 * stays visible in the same place with the same Revoke button.
 *
 * A project running in `bypassPermissions` from the user's own
 * `~/.claude/settings.json` shows as elevated but *not*
 * Claudepot-managed — we surface it, but don't offer to change
 * someone else's deliberate choice.
 */
export function PermissionPanel({
  projectPath,
  onError,
}: {
  projectPath: string;
  onError?: (msg: string) => void;
}) {
  const { t } = useTranslation("projects");
  const { perm, loading, busy, grant, extend, revert, clearIgnored } =
    useProjectPermission(projectPath, onError);
  // `null` represents the "Never" preset → sticky grant. Initial
  // value uses the first preset so common-case (time-boxed) is
  // pre-selected; users opt into sticky deliberately.
  const [durationSecs, setDurationSecs] = useState<number | null>(
    GRANT_DURATION_PRESETS[0].secs,
  );

  if (loading || !perm) {
    return (
      <section className="detail-section">
        <h3>{t("permission.heading")}</h3>
        <p className="muted small">
          {loading ? t("shared.loading") : t("permission.noData")}
        </p>
      </section>
    );
  }

  const duration = (
    <DurationSelect value={durationSecs} onChange={setDurationSecs} disabled={busy} />
  );
  return (
    <section className="detail-section">
      <h3 style={{ display: "flex", alignItems: "center", gap: "var(--sp-8)" }}>
        {t("permission.heading")}
        <PermissionTag perm={perm} />
      </h3>
      {perm.ignoredValue && (
        <IgnoredPermissionNotice perm={perm} busy={busy} onClear={clearIgnored} />
      )}
      {perm.activeGrant ? (
        <ActiveGrantControls
          perm={perm}
          durationSecs={durationSecs}
          duration={duration}
          busy={busy}
          onExtend={() => extend(durationSecs)}
          onRevert={revert}
        />
      ) : perm.isElevated ? (
        // Elevated with no Claudepot grant — the user set this in their
        // own settings file. We surface it; we don't manage it.
        <p className="muted small">
          <Trans
            ns="projects"
            i18nKey="permission.byHand"
            values={{
              mode: perm.effectiveMode,
              source: decisionLabel(perm.decidedBy),
            }}
            components={{ b: <strong /> }}
          />
        </p>
      ) : (
        <GrantPermissionForm
          duration={duration}
          busy={busy}
          onGrant={() => grant(durationSecs)}
        />
      )}
    </section>
  );
}

/** Elevated, ignored, or the effective mode — one tag beside the heading. */
function PermissionTag({ perm }: { perm: ProjectPermission }) {
  const { t } = useTranslation("projects");
  if (perm.isElevated) {
    return (
      <Tag tone="danger" glyph={NF.unlock} title={t("permission.elevatedTitle")}>
        {t("permission.elevatedTag")}
      </Tag>
    );
  }
  if (perm.ignoredValue) {
    return (
      <Tag tone="warn" glyph={NF.lock} title={t("permission.ignoredTitle")}>
        {t("permission.ignoredTag")}
      </Tag>
    );
  }
  return (
    <Tag tone="ghost" glyph={NF.lock}>
      {permissionModeLabel(perm.effectiveMode)}
    </Tag>
  );
}

/**
 * A `defaultMode` in a project file Claude Code ignores, the mode the
 * session starts in instead, and — for the local file only, which is
 * the user's — a one-click removal.
 */
function IgnoredPermissionNotice({
  perm,
  busy,
  onClear,
}: {
  perm: ProjectPermission;
  busy: boolean;
  onClear: () => void;
}) {
  const { t } = useTranslation("projects");
  const ignored = perm.ignoredValue;
  if (!ignored) return null;
  return (
    <div className="permission-ignored" role="note">
      <p className="muted small">
        <Trans
          ns="projects"
          i18nKey={
            ignored.layer === "local_project"
              ? "permission.ignoredLocal"
              : "permission.ignoredProject"
          }
          values={{
            mode: ignored.mode,
            since: perm.projectScopeIgnoresSince,
            startsIn: permissionModeLabel(perm.effectiveMode),
          }}
          components={{ b: <strong />, f: <code /> }}
        />
      </p>
      {ignored.layer === "local_project" && (
        <Button variant="outline" onClick={onClear} disabled={busy} glyph={NF.trash}>
          {t("permission.clearIgnored")}
        </Button>
      )}
    </div>
  );
}

/** A live grant: its countdown (or the missing-hook alert) and controls. */
function ActiveGrantControls({
  perm,
  durationSecs,
  duration,
  busy,
  onExtend,
  onRevert,
}: {
  perm: ProjectPermission;
  durationSecs: number | null;
  duration: ReactNode;
  busy: boolean;
  onExtend: () => void;
  onRevert: () => void;
}) {
  const { t } = useTranslation("projects");
  const g = perm.activeGrant;
  const isSticky = g != null && g.expiresAtMs == null;
  const remainingMs = useCountdown(g?.expiresAtMs ?? null);
  return (
    <div className="permission-grant-active" role={perm.hookInstalled ? "status" : "alert"}>
      {perm.hookInstalled ? (
        <span>
          {isSticky ? (
            <Trans
              ns="projects"
              i18nKey="permission.activeSticky"
              components={{ hold: <strong /> }}
            />
          ) : (
            <Trans
              ns="projects"
              i18nKey="permission.activeTimed"
              components={{
                time: <strong>{formatRemaining(remainingMs)}</strong>,
              }}
            />
          )}
        </span>
      ) : (
        // A grant CC will never be asked about. This is the one
        // state that must not read as "active": the block is the
        // alert, and the active copy is not rendered at all.
        <span className="permission-hook-missing">
          <strong>{t("permission.hookMissingTitle")}</strong>{" "}
          {t("permission.hookMissing")}
        </span>
      )}
      <div style={{ display: "flex", gap: "var(--sp-8)", alignItems: "center" }}>
        {duration}
        <Button variant="outline" onClick={onExtend} disabled={busy} glyph={NF.clock}>
          {durationSecs == null
            ? t("permission.makeSticky")
            : isSticky
              ? t("permission.setDeadline")
              : t("permission.extend")}
        </Button>
        <Button variant="solid" onClick={onRevert} disabled={busy} glyph={NF.lock}>
          {t("permission.revertNow")}
        </Button>
      </div>
    </div>
  );
}

/**
 * Milliseconds until `expiresAtMs`, re-rendered every 30s while there is
 * a deadline. A sticky grant (`null`) has no countdown and no ticker.
 */
function useCountdown(expiresAtMs: number | null): number {
  const [, setNowTick] = useState(0);
  useEffect(() => {
    if (expiresAtMs == null) return;
    const id = window.setInterval(() => setNowTick((n) => n + 1), 30_000);
    return () => window.clearInterval(id);
  }, [expiresAtMs]);
  return expiresAtMs == null ? 0 : expiresAtMs - Date.now();
}

/** No grant and nothing elevated: explain the grant, offer one. */
function GrantPermissionForm({
  duration,
  busy,
  onGrant,
}: {
  duration: ReactNode;
  busy: boolean;
  onGrant: () => void;
}) {
  const { t } = useTranslation("projects");
  return (
    <div className="permission-grant-form">
      <p className="muted small">
        <Trans
          ns="projects"
          i18nKey="permission.grantIntro"
          components={{ b1: <code />, b2: <strong /> }}
        />
      </p>
      <div style={{ display: "flex", gap: "var(--sp-8)", alignItems: "center" }}>
        {duration}
        <Button variant="solid" onClick={onGrant} disabled={busy} glyph={NF.unlock}>
          {t("permission.grantButton")}
        </Button>
      </div>
    </div>
  );
}

// Sentinel for the "Never" preset in the <select>'s string-typed
// value space — `<option value>` can't carry `null` directly.
const NEVER_SENTINEL = "never";

function DurationSelect({
  value,
  onChange,
  disabled,
}: {
  value: number | null;
  onChange: (secs: number | null) => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation("projects");
  return (
    <select
      className="mono"
      value={value == null ? NEVER_SENTINEL : String(value)}
      disabled={disabled}
      onChange={(e) =>
        onChange(e.target.value === NEVER_SENTINEL ? null : Number(e.target.value))
      }
      aria-label={t("permission.durationAria")}
      style={{
        background: "var(--bg-raised)",
        border: "var(--bw-hair) solid var(--line-strong)",
        borderRadius: "var(--r-2)",
        color: "var(--fg)",
        font: "inherit",
        fontSize: "var(--fs-sm)",
        padding: "var(--sp-4) var(--sp-8)",
      }}
    >
      {GRANT_DURATION_PRESETS.map((d) => (
        <option
          key={d.secs == null ? NEVER_SENTINEL : d.secs}
          value={d.secs == null ? NEVER_SENTINEL : String(d.secs)}
        >
          {d.label}
        </option>
      ))}
    </select>
  );
}

function decisionLabel(src: ProjectPermission["decidedBy"]): string {
  switch (src) {
    case "local_project_settings":
      return ".claude/settings.local.json";
    case "project_settings":
      return ".claude/settings.json";
    case "user_settings":
      return "~/.claude/settings.json";
    case "project_scope_ignored":
    case "default":
      return i18n.t("projects:permission.ccDefault");
  }
}
