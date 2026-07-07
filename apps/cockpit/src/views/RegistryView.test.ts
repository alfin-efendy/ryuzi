import { expect, test } from "bun:test";
import type { RegistryEntry } from "@/bindings";

import { mergeRegistryEntries } from "./RegistryView";

function entry(
  id: string,
  version: string,
  options: {
    name?: string;
    desc?: string;
    installTarget?: string;
    website?: string | null;
    publisher?: string | null;
    kind?: string;
    isLatest?: boolean;
  } = {},
): RegistryEntry {
  return {
    id,
    name: options.name ?? "Server",
    desc: options.desc ?? "A server.",
    version,
    publisher: options.publisher ?? "Acme",
    kind: options.kind ?? "stdio",
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

test("mergeRegistryEntries de-dupes by id, keeps first-seen order, and uses isLatest-aware winner", () => {
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
  expect(alpha.versions.map((v) => v.version)).toEqual(["1.1.0", "1.0.0"]);
  expect(new Set(alpha.versions.map((v) => v.version)).size).toBe(2);
});

test("mergeRegistryEntries selects top-level winner by isLatest when newer version is not top-level", () => {
  const pageOne: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "2.0.0", {
        name: "Alpha Core",
        desc: "Original top-level name",
        installTarget: "npx @alpha/2.0.0",
        website: "https://old.example",
        isLatest: false,
      }),
      versions: [
        { version: "2.0.0", installTarget: "npx @alpha/2.0.0", website: "https://old.example", isLatest: false },
      ],
    },
  ];

  const pageTwo: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "2.0.0", {
        name: "Incoming name",
        desc: "Incoming top-level name",
        installTarget: "npx @alpha/2.0.0-new",
        website: "https://incoming.example",
        isLatest: false,
      }),
      versions: [
        { version: "2.0.0", installTarget: "npx @alpha/2.0.0-new", website: "https://incoming.example", isLatest: false },
        { version: "1.9.0", installTarget: "npx @alpha/1.9.0", website: "https://latest-only.example", isLatest: true },
      ],
    },
  ];

  const merged = mergeRegistryEntries(pageOne, pageTwo);

  expect(merged).toHaveLength(1);
  expect(merged[0].version).toBe("1.9.0");
  expect(merged[0].installTarget).toBe("npx @alpha/1.9.0");
  expect(merged[0].website).toBe("https://latest-only.example");
  expect(merged[0].name).toBe("Alpha Core");
  expect(merged[0].desc).toBe("Original top-level name");
  expect(merged[0].publisher).toBe("Acme");
  expect(merged[0].versions.map((v) => v.version)).toEqual(["1.9.0", "2.0.0"]);
});
