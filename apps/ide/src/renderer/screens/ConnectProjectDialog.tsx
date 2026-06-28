import React, { useState } from "react";
import { useStore } from "../store";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function ConnectProjectDialog({ defaultOpen = false }: { defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const [gitUrl, setGitUrl] = useState("");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const setProjects = useStore((s) => s.setProjects);

  async function submit() {
    setError(null);
    const input = gitUrl.trim() ? { gitUrl: gitUrl.trim() } : name.trim() ? { name: name.trim() } : null;
    if (!input) {
      setError("Enter a git URL or a project name.");
      return;
    }
    try {
      await window.harness.connectProject(input);
      setProjects(await window.harness.listProjects());
      setGitUrl("");
      setName("");
      setOpen(false);
    } catch (e) {
      setError((e as Error).message);
    }
  }

  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Connect a project</DialogTitle>
        </DialogHeader>
        <div className="space-y-3">
          <Input placeholder="git URL (https://…/repo.git)" value={gitUrl} onChange={(e) => setGitUrl(e.target.value)} />
          <div className="text-center text-xs text-muted-foreground">or</div>
          <Input placeholder="new local project name" value={name} onChange={(e) => setName(e.target.value)} />
          {error && <p className="text-xs text-destructive">{error}</p>}
          <Button onClick={() => void submit()}>Connect</Button>
        </div>
      </DialogContent>
      <DialogTrigger asChild>
        <Button size="sm" variant="outline">
          + Connect project
        </Button>
      </DialogTrigger>
    </Dialog>
  );
}
