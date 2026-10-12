import { describe, expect, it } from "vitest";

// tokens.css hides the native disclosure triangle on every <summary>.
// A <summary> that does not draw its own chevron therefore reads as a
// plain text label: six disclosures shipped that way, including the
// rotation rule's "Advanced (guards)" section, and a user could not
// tell any of them opened. <DisclosureSummary> is the one place that
// draws the chevron, so it is the only place allowed to write <summary.

// `?raw` glob, not node:fs — this tsconfig targets the browser and has
// no Node types. Same mechanism as `registry.refresh.test.ts`.
const SOURCES = import.meta.glob("../../**/*.tsx", {
  eager: true,
  query: "?raw",
  import: "default",
}) as Record<string, string>;

const PRIMITIVE = "./modalParts.tsx";
const RAW_SUMMARY = /<summary[\s>]/;

describe("<summary> contract", () => {
  it("renders every <summary> through DisclosureSummary", () => {
    const offenders = Object.entries(SOURCES)
      .filter(([path]) => !path.includes(".test."))
      .filter(([path]) => path !== PRIMITIVE)
      .filter(([, text]) => RAW_SUMMARY.test(text))
      .map(([path]) => path);
    expect(offenders).toEqual([]);
  });

  it("still finds the primitive, so the scan is looking at real files", () => {
    expect(SOURCES[PRIMITIVE]).toMatch(RAW_SUMMARY);
    expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
  });
});
