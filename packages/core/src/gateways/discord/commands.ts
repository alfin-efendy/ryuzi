// Plain Discord application-command JSON (valid REST body; no discord.js import).
const STRING = 3; // ApplicationCommandOptionType.String

export function buildCommands(): Array<{ name: string; description: string; options?: unknown[] }> {
  return [
    {
      name: "connect",
      description: "Connect a repo (new folder by name, or clone a git URL) to a new channel",
      options: [
        { name: "name", description: "New project folder name", type: STRING, required: false },
        { name: "git", description: "Git URL to clone", type: STRING, required: false },
        { name: "model", description: "Model override", type: STRING, required: false },
        { name: "effort", description: "Reasoning effort", type: STRING, required: false },
        {
          name: "mode",
          description: "Permission mode",
          type: STRING,
          required: false,
          choices: [
            { name: "default", value: "default" },
            { name: "acceptEdits", value: "acceptEdits" },
            { name: "bypassPermissions", value: "bypassPermissions" },
          ],
        },
      ],
    },
    { name: "end", description: "End the session in this thread (removes its worktree)" },
    { name: "stop", description: "Stop the running turn in this thread" },
    { name: "status", description: "Show harness status" },
  ];
}
