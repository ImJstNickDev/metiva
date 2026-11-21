import { useEffect, useState } from "react";

export function useTheme() {
  const [theme, setTheme] = useState(() => {
    try {
      return localStorage.getItem("metiva-theme") === "light"
        ? "light"
        : "dark";
    } catch {
      return "dark";
    }
  });
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    try {
      localStorage.setItem("metiva-theme", theme);
    } catch {
      /* Theme remains available for this visit. */
    }
  }, [theme]);
  return [theme, () => setTheme((current) => current === "dark" ? "light" : "dark")] as const;
}
