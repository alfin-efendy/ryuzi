import { test, expect } from "bun:test";
import { createControlPlaneClient } from "../src/index";

test("listDir/readFile POST /rpc with the right method + params", async () => {
  const calls: any[] = [];
  const fetchImpl = (async (_url: string, init: any) => {
    const body = JSON.parse(init.body);
    calls.push(body);
    const result =
      body.method === "listDir" ? [{ name: "a.txt", type: "file" }] : { content: "hi", encoding: "utf8", binary: false, truncated: false };
    return new Response(JSON.stringify({ id: body.id, ok: true, result }), {
      status: 200,
    });
  }) as unknown as typeof fetch;
  const client = createControlPlaneClient({
    baseUrl: "http://x",
    getToken: async () => "t",
    fetchImpl,
  });
  expect(await client.listDir({ sessionPk: "s1", path: "" })).toEqual([{ name: "a.txt", type: "file" }]);
  expect((await client.readFile({ sessionPk: "s1", path: "a.txt" })).content).toBe("hi");
  expect(calls.map((c) => c.method)).toEqual(["listDir", "readFile"]);
  expect(calls[0].params).toEqual({ sessionPk: "s1", path: "" });
});
