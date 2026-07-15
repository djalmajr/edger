import { describe, it, expect } from "vitest";
import { render, screen } from "@testing-library/react";
import { Badge } from "./badge";

describe("Badge", () => {
  // Mutation captured: collapsing the variant→class map (e.g. always using the
  // default style) would put bg-primary on a destructive badge → fails.
  it("maps each variant to its style class", () => {
    render(
      <>
        <Badge>def</Badge>
        <Badge variant="secondary">sec</Badge>
        <Badge variant="destructive">des</Badge>
      </>,
    );
    expect(screen.getByText("def").className).toContain("bg-primary");
    expect(screen.getByText("sec").className).toContain("bg-secondary");
    expect(screen.getByText("des").className).toContain("bg-destructive");
  });
});
