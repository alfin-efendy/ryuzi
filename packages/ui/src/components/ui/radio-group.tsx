import { Radio, type RadioRootProps } from "@base-ui/react/radio";
import { RadioGroup as RadioGroupPrimitive, type RadioGroupProps } from "@base-ui/react/radio-group";
import * as React from "react";
import { cn } from "../../lib/utils";
import { Field, FieldContent, FieldDescription, FieldLabel } from "./field";

function RadioGroup({ className, ...props }: RadioGroupProps<string>) {
  return <RadioGroupPrimitive data-slot="radio-group" className={cn("grid gap-2", className)} {...props} />;
}

function RadioGroupItem({ className, ...props }: RadioRootProps<string>) {
  return (
    <Radio.Root
      data-slot="radio-group-item"
      className={cn(
        "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border border-input text-primary outline-none",
        "data-checked:border-primary focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50",
        className,
      )}
      {...props}
    >
      <Radio.Indicator className="size-2 rounded-full bg-current" />
    </Radio.Root>
  );
}

type ChoiceCardProps = {
  value: string;
  title: React.ReactNode;
  description?: React.ReactNode;
  leading?: React.ReactNode;
  disabled?: boolean;
  className?: string;
};

function ChoiceCard({ value, title, description, leading, disabled = false, className }: ChoiceCardProps) {
  const id = React.useId();
  return (
    <FieldLabel
      htmlFor={id}
      data-disabled={disabled || undefined}
      className={cn(
        "block cursor-pointer rounded-lg border border-border text-left has-[[data-checked]]:border-primary has-[[data-checked]]:bg-accent/60 data-disabled:cursor-not-allowed data-disabled:opacity-50",
        className,
      )}
    >
      <Field className="items-start px-3 py-3">
        {leading}
        <FieldContent>
          <span className="block text-[13px] font-semibold text-foreground">{title}</span>
          {description !== undefined && <FieldDescription>{description}</FieldDescription>}
        </FieldContent>
        <RadioGroupItem id={id} value={value} disabled={disabled} className="ml-auto" />
      </Field>
    </FieldLabel>
  );
}

export { ChoiceCard, RadioGroup, RadioGroupItem };
export type { ChoiceCardProps };
