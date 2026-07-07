import { expect, test } from "bun:test";
import type { RegistryEntry } from "@/bindings";

import { mergeRegistryEntries } from "./RegistryView";

function entry(
  id: string,
  version: string,
  options: { desc?: string; installTarget?: string; website?: string | null; isLatest?: boolean } = {},
): RegistryEntry {
  return {
    id,
    name: "Server",
    desc: options.desc ?? "A server.",
    version,
    publisher: "Acme",
    kind: "stdio",
    installTarget: options.installTarget ?? `npx ${id}@${version}`,
    website: options.website ?? null,
    versions: [
      {
        version,
        installTarget: options.installTarget ?? `npx ${id}@${version}`,
        website: options.website ?? null,
        isLatest: options.isLatest ?? false,
      },
    ],
  };
}

test("mergeRegistryEntries de-dupes by id, keeps first-seen entry order, and keeps latest top-level fields", () => {
  const pageOne: RegistryEntry[] = [
    { ...entry("io.github/org/alpha", "1.0.0", { desc: "alpha old", installTarget: "npx @alpha/1.0.0", isLatest: false }) },
    { ...entry("io.github/org/beta", "0.9.0", { installTarget: "npx @beta/0.9.0" }) },
  ];
  const pageTwo: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "1.1.0", {
        desc: "alpha latest",
        installTarget: "npx @alpha/1.1.0",
        website: "https://alpha.example",
        isLatest: true,
      }),
      versions: [
        { version: "1.1.0", installTarget: "npx @alpha/1.1.0", website: "https://alpha.example", isLatest: true },
      ],
    },
    { ...entry("io.github/org/gamma", "2.0.0", { installTarget: "npx @gamma/2.0.0" }) },
  ];

  const merged = mergeRegistryEntries(pageOne, pageTwo);

  expect(merged).toHaveLength(3);
  expect(merged.map((row) => row.id)).toEqual(["io.github/org/alpha", "io.github/org/beta", "io.github/org/gamma"]);

  const alpha = merged[0];
  expect(alpha.version).toBe("1.1.0");
  expect(alpha.desc).toBe("alpha latest");
  expect(alpha.installTarget).toBe("npx @alpha/1.1.0");
  expect(alpha.website).toBe("https://alpha.example");
  expect(alpha.versions.map((v) => v.version)).toEqual(["1.0.0", "1.1.0"]);
  expect(new Set(alpha.versions.map((v) => v.version)).size).toBe(2);
});
