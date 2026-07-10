import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AddSkillSourceModal } from "./AddSkillSourceModal";
import { useSkills } from "@/store-skills";

afterEach(cleanup);

test("installs the typed source and closes on success", async () => {
  const installSource = mock(async (_s: string) => true);
  useSkills.setState({ ...useSkills.getState(), installSource, loading: false });
  const onClose = mock(() => {});
  render(<AddSkillSourceModal onClose={onClose} />);

  fireEvent.change(screen.getByLabelText("Skill source"), { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install" }));

  await waitFor(() => expect(onClose).toHaveBeenCalled());
  expect(installSource).toHaveBeenCalledWith("obra/superpowers");
});

test("stays open when install fails", async () => {
  const installSource = mock(async (_s: string) => false);
  useSkills.setState({ ...useSkills.getState(), installSource, loading: false });
  const onClose = mock(() => {});
  render(<AddSkillSourceModal onClose={onClose} />);

  fireEvent.change(screen.getByLabelText("Skill source"), { target: { value: "bad/repo" } });
  fireEvent.click(screen.getByRole("button", { name: "Install" }));

  await waitFor(() => expect(installSource).toHaveBeenCalled());
  expect(onClose).not.toHaveBeenCalled();
});
