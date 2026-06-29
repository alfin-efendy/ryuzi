import { test, expect } from "bun:test";
import { discordGateway } from "../../src/providers/gateways/discord";
import type { InboundRouter } from "../../src/gateways/discord/index";

test("discord descriptor declares namespaced required fields", () => {
  expect(discordGateway.id).toBe("discord");
  expect(discordGateway.kind).toBe("gateway");
  const keys = discordGateway.fields.map((f) => f.key);
  expect(keys).toEqual(["discord.token", "discord.app_id", "discord.guild_id"]);
  expect(discordGateway.fields.find((f) => f.key === "discord.token")!.secret).toBe(true);
  expect(discordGateway.fields.every((f) => f.required && f.help.length > 0)).toBe(true);
});

test("discord descriptor builds a gateway from config", () => {
  const gw = discordGateway.build(
    { "discord.token": "t", "discord.app_id": "a", "discord.guild_id": "g" },
    { router: {} as InboundRouter },
  );
  expect(gw.id).toBe("discord");
});
