// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useRef } from "react";
import { afterEach, expect, it, vi } from "vitest";
import { BackToTop } from "./BackToTop";

function Dashboard() {
  const target = useRef<HTMLElement>(null);
  return (
    <>
      <BackToTop target={target} />
      <main ref={target} tabIndex={-1}>
        <h1>Dashboard</h1>
      </main>
    </>
  );
}

function mount(reducedMotion: boolean) {
  vi.stubGlobal("scrollY", 0);
  vi.stubGlobal("innerHeight", 800);
  vi.stubGlobal("matchMedia", () => ({ matches: reducedMotion }));
  vi.stubGlobal("scrollTo", ({ top }: ScrollToOptions) => {
    vi.stubGlobal("scrollY", top);
    window.dispatchEvent(new Event("scroll"));
  });
  render(<Dashboard />);
}

function scrollTo(top: number) {
  act(() => window.scrollTo({ top }));
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

it("appears after one viewport and quickly animates the return from a long list", () => {
  vi.useFakeTimers({
    toFake: ["requestAnimationFrame", "cancelAnimationFrame", "performance"],
  });
  mount(false);
  scrollTo(800);
  expect(screen.queryByRole("button", { name: "Back to top" })).toBeNull();

  const distance = 100_000;
  scrollTo(distance);
  const button = screen.getByRole("button", { name: "Back to top" });
  button.focus();
  fireEvent.click(button);
  expect(document.activeElement).toBe(screen.getByRole("main"));
  expect(window.scrollY).toBe(distance);

  act(() => vi.advanceTimersByTime(100));
  expect(window.scrollY).toBeGreaterThan(0);
  expect(window.scrollY).toBeLessThan(distance);

  act(() => vi.advanceTimersByTime(300));
  expect(window.scrollY).toBe(0);
  expect(screen.queryByRole("button", { name: "Back to top" })).toBeNull();
});

it("returns to the top immediately with reduced motion", () => {
  mount(true);
  scrollTo(50_000);

  fireEvent.click(screen.getByRole("button", { name: "Back to top" }));
  expect(window.scrollY).toBe(0);
  expect(document.activeElement).toBe(screen.getByRole("main"));
  expect(screen.queryByRole("button", { name: "Back to top" })).toBeNull();
});
