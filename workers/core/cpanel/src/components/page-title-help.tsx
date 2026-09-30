import { Button } from "@edger/ui/components/ui/button";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@edger/ui/components/ui/tooltip";
import CircleHelpIcon from "~icons/lucide/circle-help";
import { useId } from "react";

// Use the shared tooltip primitive for hover and keyboard focus help.
export function PageTitleHelp({
  content,
  label,
}: {
  content: string;
  label: string;
}) {
  const descriptionId = useId();
  return (
    <Tooltip>
      <TooltipTrigger
        aria-describedby={descriptionId}
        render={
          <Button
            aria-label={label}
            size="icon-sm"
            type="button"
            variant="ghost"
          >
            <CircleHelpIcon aria-hidden />
          </Button>
        }
      />
      <TooltipContent
        className="whitespace-normal"
        id={descriptionId}
        role="tooltip"
        side="bottom"
      >
        {content}
      </TooltipContent>
    </Tooltip>
  );
}
