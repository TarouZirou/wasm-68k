import { defineConfig, devices } from "@playwright/test";

const port = Number(process.env.PLAYWRIGHT_PORT ?? 4173);
const origin = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests",
  timeout: 60_000,
  expect: { timeout: 30_000 },
  use: {
    baseURL: origin,
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        launchOptions: {
          args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"],
        },
      },
    },
  ],
  webServer: {
    // 本番成果物と同じbaseでpreviewし、/wasm-68k/配下を実際に検証する。
    command: `GITHUB_PAGES=true npm run preview -- --host 127.0.0.1 --port ${port}`,
    url: `${origin}/wasm-68k/`,
    reuseExistingServer: !process.env.CI,
  },
});
