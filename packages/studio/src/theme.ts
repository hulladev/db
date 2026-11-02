// Run before CSS paints. Preferences are separate from operator credentials.
try {
  const preference = localStorage.getItem("hulla-studio-theme");
  document.documentElement.dataset.theme = preference === "light" || preference === "dark"
    ? preference : matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
} catch { document.documentElement.dataset.theme = "light"; }
