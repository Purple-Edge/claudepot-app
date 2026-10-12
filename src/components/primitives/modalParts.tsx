import {
  useId,
  type ChangeEvent,
  type CSSProperties,
  type ReactNode,
} from "react";
import { Glyph } from "./Glyph";
import { NF } from "../../icons";

/**
 * Shared paper-mono primitives for the modal interior. Each one is a
 * thin, style-only React component that leans on the design-token
 * layer in `tokens.css`. Extracted out of `RenameProjectModal` so the
 * same shape (label stack, bordered group, bordered danger zone,
 * multi-line consequence rows, collapsible advanced section) renders
 * identically across Rename/Move/Adopt/etc. modals.
 *
 * Not placed in every-primitive bucket (`Button`, `Modal`, etc.)
 * because these are composition helpers for modal bodies, not first-
 * class UI atoms. Keeping them in a dedicated file makes it easy to
 * find all of them at once when the modal visual language shifts.
 */

// ── Label + content stack ────────────────────────────────────────

/** Label + control stack. Keeps the `<label htmlFor>` association
 *  intact for tests and screen readers; visual label style is the
 *  global `mono-cap` uppercase token in `--fg-faint`. */
export function FieldBlock({
  label,
  htmlFor,
  children,
}: {
  label: ReactNode;
  htmlFor?: string;
  children: ReactNode;
}) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: "var(--sp-6)" }}>
      <label
        htmlFor={htmlFor}
        className="mono-cap"
        style={{ color: "var(--fg-faint)", fontSize: "var(--fs-2xs)" }}
      >
        {label}
      </label>
      {children}
    </div>
  );
}

// ── Bordered group ───────────────────────────────────────────────

/** Bordered group — replaces `<fieldset>`/`<legend>` so the legend
 *  never punches through the border and the layout stays column-
 *  stacked. Use `tone="danger"` for red-tinted consequence groups. */
export function GroupCard({
  label,
  children,
  tone = "neutral",
}: {
  label: ReactNode;
  children: ReactNode;
  tone?: "neutral" | "danger";
}) {
  const border = tone === "danger" ? "var(--danger)" : "var(--line)";
  const background = tone === "danger" ? "var(--bad-weak)" : "transparent";
  return (
    <div
      style={{
        display: "flex",
        flexDirection: "column",
        gap: "var(--sp-8)",
        padding: "var(--sp-10) var(--sp-12)",
        border: `var(--bw-hair) solid ${border}`,
        borderRadius: "var(--r-2)",
        background,
      }}
    >
      <div
        className="mono-cap"
        style={{ fontSize: "var(--fs-2xs)", color: "var(--fg-faint)" }}
      >
        {label}
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: "var(--sp-6)" }}>
        {children}
      </div>
    </div>
  );
}

// ── Muted helper text ────────────────────────────────────────────

/** Small muted helper text. Use below an input or next to a group
 *  to attach a one-sentence hint that isn't load-bearing enough to
 *  go into the label itself. */
export function Hint({ children }: { children: ReactNode }) {
  return (
    <p
      style={{
        margin: 0,
        fontSize: "var(--fs-xs)",
        color: "var(--fg-faint)",
      }}
    >
      {children}
    </p>
  );
}

// ── Radio / checkbox row ─────────────────────────────────────────

/** Radio or checkbox row with a wrapping label. Children compose
 *  the visible label — both the headline verb and its consequence
 *  hint live inside a single inline run so the text wraps as one
 *  block instead of snapping into narrow flex columns. */
export function OptionRow({
  type,
  name,
  checked,
  onChange,
  disabled,
  children,
}: {
  type: "radio" | "checkbox";
  name?: string;
  checked: boolean;
  onChange: (e: ChangeEvent<HTMLInputElement>) => void;
  disabled?: boolean;
  children: ReactNode;
}) {
  return (
    <label
      style={{
        display: "flex",
        alignItems: "flex-start",
        gap: "var(--sp-8)",
        fontSize: "var(--fs-sm)",
        color: disabled ? "var(--fg-faint)" : "var(--fg)",
        lineHeight: "var(--lh-body)",
      }}
    >
      <input
        type={type}
        name={name}
        checked={checked}
        onChange={onChange}
        disabled={disabled}
        className="pm-focus"
        style={{
          marginTop: "var(--sp-3)",
          flexShrink: 0,
          cursor: "inherit",
        }}
      />
      <span style={{ minWidth: 0, flex: 1 }}>{children}</span>
    </label>
  );
}

// ── Collapsible advanced section ─────────────────────────────────

/** The one way to render a `<summary>`. tokens.css suppresses the
 *  native disclosure triangle globally, so a bare `<summary>` reads as
 *  a plain text label; this draws a chevron that rotates on `[open]`
 *  (base.css). Styling the label stays with the caller. */
export function DisclosureSummary({
  children,
  style,
  "aria-controls": ariaControls,
}: {
  children: ReactNode;
  style?: CSSProperties;
  "aria-controls"?: string;
}) {
  return (
    <summary
      className="disclosure-summary"
      aria-controls={ariaControls}
      style={style}
    >
      <Glyph g={NF.chevronR} className="disclosure-chevron" />
      <span>{children}</span>
    </summary>
  );
}

/** Collapsible group, rendered as a styled `<details>` so native
 *  keyboard + aria-expanded support come for free. */
export function Disclosure({
  label,
  children,
  defaultOpen = false,
}: {
  label: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
}) {
  const panelId = useId();
  return (
    <details
      open={defaultOpen}
      style={{
        borderTop: "var(--bw-hair) solid var(--line)",
        paddingTop: "var(--sp-8)",
      }}
    >
      <DisclosureSummary
        aria-controls={panelId}
        style={{
          padding: "var(--sp-4) 0",
          fontSize: "var(--fs-xs)",
          color: "var(--fg-muted)",
        }}
      >
        {label}
      </DisclosureSummary>
      <div
        id={panelId}
        style={{
          display: "flex",
          flexDirection: "column",
          gap: "var(--sp-8)",
          padding: "var(--sp-8) 0 var(--sp-4)",
        }}
      >
        {children}
      </div>
    </details>
  );
}
