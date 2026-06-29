import type { GatewayDescriptor } from "../types";
import { DiscordClientPort } from "../../gateways/discord/client-port";
import { DiscordGateway } from "../../gateways/discord/index";

export const discordGateway: GatewayDescriptor = {
  id: "discord",
  label: "Discord",
  description: "Drive sessions from a Discord server",
  kind: "gateway",
  fields: [
    {
      key: "discord.token",
      label: "Bot token",
      secret: true,
      required: true,
      help: "Discord Developer Portal -> your app -> Bot -> Reset Token",
      example: "MTk4Nj...long.secret",
    },
    {
      key: "discord.app_id",
      label: "Application ID",
      required: true,
      help: "Developer Portal -> General Information -> Application ID",
      example: "123456789012345678",
    },
    {
      key: "discord.guild_id",
      label: "Server (guild) ID",
      required: true,
      help: "Enable Developer Mode, right-click your server -> Copy Server ID",
      example: "987654321098765432",
    },
  ],
  build(cfg, ctx) {
    const port = new DiscordClientPort({
      token: cfg["discord.token"]!,
      appId: cfg["discord.app_id"]!,
      guildId: cfg["discord.guild_id"]!,
    });
    return new DiscordGateway(port, ctx.router);
  },
};
