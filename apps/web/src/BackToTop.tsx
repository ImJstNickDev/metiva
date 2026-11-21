import { useEffect, useRef, useState, type RefObject } from "react";
import { ArrowUp } from "lucide-react";
import "./BackToTop.css";

const scrollDuration = 380;

export function BackToTop({
  target,
}: {
  target: RefObject<HTMLElement | null>;
}) {
  const [visible, setVisible] = useState(false);
  const frame = useRef<number | null>(null);

  function cancelScroll() {
    if (frame.current === null) return;
    window.cancelAnimationFrame(frame.current);
    frame.current = null;
  }

  useEffect(() => {
    const update = () => setVisible(window.scrollY > window.innerHeight);
    update();
    window.addEventListener("scroll", update, { passive: true });
    window.addEventListener("resize", update);
    window.addEventListener("wheel", cancelScroll, { passive: true });
    window.addEventListener("pointerdown", cancelScroll, { passive: true });
    window.addEventListener("keydown", cancelScroll);
    return () => {
      window.removeEventListener("scroll", update);
      window.removeEventListener("resize", update);
      window.removeEventListener("wheel", cancelScroll);
      window.removeEventListener("pointerdown", cancelScroll);
      window.removeEventListener("keydown", cancelScroll);
      cancelScroll();
    };
  }, []);

  function goToTop() {
    cancelScroll();
    target.current?.focus({ preventScroll: true });
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) {
      window.scrollTo({ top: 0, behavior: "instant" });
      return;
    }
    const distance = window.scrollY;
    const start = performance.now();
    const step = (now: number) => {
      const elapsed = Math.max(now - start, 0);
      const progress = Math.min(elapsed / scrollDuration, 1);
      const remaining = Math.round(distance * (1 - progress) ** 3);
      window.scrollTo({ top: remaining, behavior: "instant" });
      frame.current = remaining > 0 ? window.requestAnimationFrame(step) : null;
    };
    frame.current = window.requestAnimationFrame(step);
  }

  if (!visible) return null;
  return (
    <button type="button" className="btn back-to-top" onClick={goToTop}>
      <ArrowUp aria-hidden="true" />
      Back to top
    </button>
  );
}
