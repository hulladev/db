import hljs from "highlight.js/lib/core";
import json from "highlight.js/lib/languages/json";

// Only JSON is shipped. Keep this module behind the Schema tab's dynamic import.
hljs.registerLanguage("json", json);
export const highlightJSON = (source: string): string => hljs.highlight(source, { language: "json", ignoreIllegals: true }).value;
