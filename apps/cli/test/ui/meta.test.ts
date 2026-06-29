import { test, expect } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { brandAssets, brandGlyph, brandName } from "../../src/cli/brand";
import { helpText, version } from "../../src/cli/meta";

test("help leads with OPTIONS and lists only doctor + run", () => {
  const h = helpText();
  expect(h).toContain("Harness Router");
  expect(h).toContain("OPTIONS");
  expect(h).toContain("doctor");
  expect(h).toContain("run");
  expect(h).not.toContain("config"); // hidden
  expect(h).not.toContain("init");
  expect(h).not.toContain("start");
});

test("version is a semver-ish string", () => {
  expect(version()).toMatch(/\d+\.\d+\.\d+/);
});

test("brand metadata points to packaged logo assets", () => {
  expect(brandGlyph).toBe("マ");
  expect(brandName).toBe("Harness Router");
  expect(Object.keys(brandAssets).sort()).toEqual([
    "faviconIco",
    "iconPng",
    "markAdaptiveSvg",
    "markDarkSvg",
    "markLightSvg",
    "markSolidSvg",
    "markSvg",
    "wordmarkAdaptiveSvg",
    "wordmarkDarkSvg",
    "wordmarkLightSvg",
    "wordmarkSvg",
  ]);
  for (const asset of Object.values(brandAssets)) {
    const path = fileURLToPath(asset);
    expect(path).toContain("/assets/brand/");
    expect(path).not.toContain("/assets/brand/harness-router/");
    expect(path).not.toContain("/apps/cli/assets/");
    expect(existsSync(path)).toBe(true);
  }
});

test("brand usage guide documents the shared root assets", () => {
  const guidePath = fileURLToPath(new URL("../../../../assets/brand/README.md", import.meta.url));
  const guide = readFileSync(guidePath, "utf8");
  expect(guide).toContain("wordmark.svg");
  expect(guide).toContain("mark.svg");
  expect(guide).toContain("<picture>");
  expect(guide).toContain("prefers-color-scheme");
  expect(guide).toContain("outputs/logos/");
  expect(guide).not.toContain("assets/brand/harness-router");
});

test("root readme uses explicit light and dark wordmark sources", () => {
  const readmePath = fileURLToPath(new URL("../../../../README.md", import.meta.url));
  const readme = readFileSync(readmePath, "utf8");
  expect(readme).toContain("<picture>");
  expect(readme).toContain('media="(prefers-color-scheme: dark)"');
  expect(readme).toContain("assets/brand/wordmark-dark.svg");
  expect(readme).toContain("assets/brand/wordmark-light.svg");
  expect(readme).not.toContain("![Harness Router wordmark](assets/brand/wordmark.svg)");
});

test("brand svg assets are light, dark, and adaptive safe", () => {
  const wordmark = readFileSync(fileURLToPath(brandAssets.wordmarkSvg), "utf8");
  const mark = readFileSync(fileURLToPath(brandAssets.markSvg), "utf8");
  const wordmarkAdaptive = readFileSync(fileURLToPath(brandAssets.wordmarkAdaptiveSvg), "utf8");
  const markAdaptive = readFileSync(fileURLToPath(brandAssets.markAdaptiveSvg), "utf8");
  const wordmarkLight = readFileSync(fileURLToPath(brandAssets.wordmarkLightSvg), "utf8");
  const wordmarkDark = readFileSync(fileURLToPath(brandAssets.wordmarkDarkSvg), "utf8");

  expect(wordmark).toContain("prefers-color-scheme");
  expect(mark).toContain("prefers-color-scheme");
  expect(wordmarkAdaptive).toContain("prefers-color-scheme");
  expect(markAdaptive).toContain("prefers-color-scheme");
  expect(wordmark).not.toContain('width="1600" height="420" fill="#ffffff"');
  expect(wordmarkLight).toContain('fill="#050505"');
  expect(wordmarkDark).toContain('fill="#ffffff"');
  expect(wordmarkLight).not.toContain('width="1600" height="420" fill="#ffffff"');
  expect(wordmarkDark).not.toContain('width="1600" height="420" fill="#050505"');
});
