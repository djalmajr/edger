import { Switch as SwitchPrimitive } from "@base-ui/react/switch";

import { cn } from "@edger/ui/lib/utils";

/** Switch on/off (Base UI / shadcn base-nova). Controlado via checked + onCheckedChange. */
function Switch({ className, ...props }: SwitchPrimitive.Root.Props) {
  return (
    <SwitchPrimitive.Root
      data-slot="switch"
      className={cn(
        "peer inline-flex h-[1.15rem] w-8 shrink-0 cursor-pointer items-center rounded-sm border border-transparent shadow-xs transition-all outline-none focus-visible:border-ring focus-visible:ring-[3px] focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50 data-[checked]:bg-primary data-[unchecked]:bg-input dark:data-[unchecked]:bg-input/80",
        className,
      )}
      {...props}
    >
      <SwitchPrimitive.Thumb
        data-slot="switch-thumb"
        className="pointer-events-none block size-[15px] rounded-[3px] bg-background ring-0 transition-transform data-[checked]:translate-x-[calc(100%-1px)] data-[unchecked]:translate-x-px dark:data-[checked]:bg-primary-foreground dark:data-[unchecked]:bg-foreground"
      />
    </SwitchPrimitive.Root>
  );
}

export { Switch };
