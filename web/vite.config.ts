import { readFileSync } from "node:fs";
import { defineConfig } from "vite";

export default defineConfig({
  base: process.env.GITHUB_PAGES === "true" ? "/wasm-68k/" : "/",
  plugins: [
    {
      name: "include-license",
      /** 公開成果物へライセンスと第三者通知を改変せず同梱する。 */
      generateBundle() {
        for (const fileName of ["LICENSE", "../LICENSE-EXCEPTION", "../THIRD_PARTY_NOTICES.md"]) {
          this.emitFile({
            type: "asset",
            fileName: fileName.split("/").at(-1)!,
            source: readFileSync(new URL(fileName, import.meta.url), "utf8"),
          });
        }
      },
    },
  ],
  build: {
    target: "es2022",
    sourcemap: true,
  },
});
