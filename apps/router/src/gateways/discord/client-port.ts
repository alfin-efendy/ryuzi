// apps/router/src/gateways/discord/client-port.ts
import {
  Client,
  Events,
  GatewayIntentBits,
  ChannelType,
  REST,
  Routes,
  MessageFlags,
  ActionRowBuilder,
  ButtonBuilder,
  ButtonStyle,
  ComponentType,
  GuildMember,
  type TextChannel,
  type ThreadChannel,
} from "discord.js";
import type { DiscordPort, InboundMessage, InboundInteraction } from "./index";
import { buildCommands } from "./commands";
import { canApprove } from "../../core/permissions";

export async function registerCommands(token: string, appId: string, guildId: string): Promise<void> {
  const rest = new REST({ version: "10" }).setToken(token);
  await rest.put(Routes.applicationGuildCommands(appId, guildId), { body: buildCommands() });
}

export class DiscordClientPort implements DiscordPort {
  private client: Client;
  constructor(private opts: { token: string; appId: string; guildId: string }) {
    this.client = new Client({
      intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildMessages, GatewayIntentBits.MessageContent],
    });
  }

  botUserId(): string | undefined {
    return this.client.user?.id;
  }

  async connect(handlers: {
    onMessage: (e: InboundMessage) => Promise<void>;
    onInteraction: (e: InboundInteraction, reply: (text: string) => Promise<void>) => Promise<void>;
  }): Promise<void> {
    await registerCommands(this.opts.token, this.opts.appId, this.opts.guildId);

    this.client.on(Events.MessageCreate, (msg) => {
      void handlers.onMessage({
        channelId: msg.channelId,
        isThread: msg.channel.isThread(),
        authorBot: msg.author.bot,
        authorId: msg.author.id,
        mentionsBot: this.client.user ? msg.mentions.has(this.client.user) : false,
        content: msg.content,
        attachments: [...msg.attachments.values()].map((a) => ({
          name: a.name ?? "file",
          url: a.url,
          contentType: a.contentType ?? undefined,
          size: a.size,
        })),
      });
    });

    this.client.on(Events.InteractionCreate, async (interaction) => {
      if (!interaction.isChatInputCommand()) return;
      await interaction.deferReply({ flags: MessageFlags.Ephemeral });
      await handlers.onInteraction(
        {
          name: interaction.commandName,
          userId: interaction.user.id,
          channelId: interaction.channelId ?? "",
          options: {
            name: interaction.options.getString("name") ?? undefined,
            git: interaction.options.getString("git") ?? undefined,
            model: interaction.options.getString("model") ?? undefined,
            effort: interaction.options.getString("effort") ?? undefined,
            mode: interaction.options.getString("mode") ?? undefined,
          },
        },
        async (text) => {
          await interaction.editReply(text);
        },
      );
    });

    await new Promise<void>((resolve) => {
      this.client.once(Events.ClientReady, () => resolve());
      void this.client.login(this.opts.token);
    });
  }

  async disconnect(): Promise<void> {
    await this.client.destroy();
  }

  async createTextChannel(name: string): Promise<string> {
    const guild = await this.client.guilds.fetch(this.opts.guildId);
    const channel = await guild.channels.create({ name, type: ChannelType.GuildText });
    return channel.id;
  }
  async createThread(channelId: string, name: string): Promise<string> {
    const channel = (await this.client.channels.fetch(channelId)) as TextChannel | null;
    if (!channel) throw new Error(`channel not found: ${channelId}`);
    const thread = await channel.threads.create({ name });
    return thread.id;
  }
  async sendMessage(channelId: string, text: string): Promise<string> {
    const channel = (await this.client.channels.fetch(channelId)) as TextChannel | ThreadChannel | null;
    if (!channel) throw new Error(`channel not found: ${channelId}`);
    const message = await channel.send(text);
    return message.id;
  }
  async editMessage(channelId: string, messageId: string, text: string): Promise<void> {
    const channel = (await this.client.channels.fetch(channelId)) as TextChannel | ThreadChannel | null;
    if (!channel) return;
    const message = await channel.messages.fetch(messageId);
    await message.edit(text);
  }

  async requestApproval(
    conversationId: string,
    req: {
      requestId: string;
      tool: string;
      summary: string;
      approverRoleIds: string[];
      startedBy?: string;
      timeoutMs: number;
    },
  ): Promise<{ decision: "allow" | "deny"; actor: string }> {
    const channel = (await this.client.channels.fetch(conversationId)) as TextChannel | ThreadChannel | null;
    if (!channel) return { decision: "deny", actor: "no-channel" };
    const row = new ActionRowBuilder<ButtonBuilder>().addComponents(
      new ButtonBuilder().setCustomId(`${req.requestId}:approve`).setLabel("Approve").setStyle(ButtonStyle.Success),
      new ButtonBuilder().setCustomId(`${req.requestId}:deny`).setLabel("Deny").setStyle(ButtonStyle.Danger),
    );
    const message = await channel.send({ content: `🔐 Approve **${req.tool}**?\n\`\`\`\n${req.summary}\n\`\`\``, components: [row] });

    return await new Promise<{ decision: "allow" | "deny"; actor: string }>((resolve) => {
      let settled = false;
      const collector = message.createMessageComponentCollector({ componentType: ComponentType.Button, time: req.timeoutMs });
      collector.on("collect", async (i) => {
        if (settled) return;
        try {
          const clickerRoleIds = i.member instanceof GuildMember ? [...i.member.roles.cache.keys()] : [];
          const allowed = canApprove({ clickerRoleIds, approverRoleIds: req.approverRoleIds, isStarter: i.user.id === req.startedBy });
          if (!allowed) {
            await i.reply({ content: "You are not authorized to approve this.", flags: MessageFlags.Ephemeral }).catch(() => {});
            return;
          }
          const decision = i.customId.endsWith(":approve") ? "allow" : i.customId.endsWith(":deny") ? "deny" : null;
          if (decision === null) return; // unexpected customId → ignore (fail-closed; collector end denies)
          settled = true;
          collector.stop("done");
          resolve({ decision, actor: i.user.id }); // lock the decision BEFORE the fallible UI edit
          await i
            .update({
              content: `${decision === "allow" ? "✅ Approved" : "🚫 Denied"} by <@${i.user.id}> — **${req.tool}**`,
              components: [],
            })
            .catch(() => {});
        } catch {
          // a rejected interaction must never crash the daemon; if unsettled, collector 'end' will deny
        }
      });
      collector.on("end", () => {
        if (!settled) resolve({ decision: "deny", actor: "timeout" });
      });
    });
  }
}
