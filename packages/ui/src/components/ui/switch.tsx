import { cn } from "../../lib/utils";

type SwitchProps = {
  on: boolean;
  onToggle: () => void;
  size?: "md" | "lg";
  label?: string;
};

// Design toggle: pill track with a sliding knob (36×21 default, 40×23 large).
function Switch({ on, onToggle, size = "md", label }: SwitchProps) {
  const lg = size === "lg";
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={onToggle}
      className={cn(
        "relative shrink-0 cursor-pointer rounded-full border-none p-0 transition-colors duration-150",
        lg ? "h-[23px] w-10" : "h-[21px] w-9",
      )}
      style={{ background: on ? "var(--primary)" : "var(--input)" }}
    >
      <span
        className={cn(
          "absolute top-[2.5px] rounded-full bg-primary-foreground shadow-sm transition-[left] duration-150",
          lg ? "h-[18px] w-[18px]" : "h-4 w-4",
        )}
        style={{ left: on ? (lg ? "19px" : "17px") : lg ? "2.5px" : "3px" }}
      />
    </button>
  );
}

export { Switch };
