document.addEventListener("DOMContentLoaded", async () => {
  const dark = matchMedia("(prefers-color-scheme: dark)").matches;
  mermaid.initialize({ startOnLoad: false, theme: dark ? "dark" : "neutral", flowchart: { curve: "basis" } });
  try { await mermaid.run({ querySelector: ".mermaid" }); }
  catch (e) { console.error("mermaid", e); }
});
