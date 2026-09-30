import js from "@eslint/js";
import tseslint from "typescript-eslint";
import sonarjs from "eslint-plugin-sonarjs";

// Quality gates for the shipped runtime: app/anamnesis and packages/*/src.
export default tseslint.config(
  { ignores: ["dist/**", "node_modules/**", ".omo/**", "scripts/**", "**/*.mjs", "**/*.cjs", "**/*.test.ts", "**/*.fixture.ts"] },
  js.configs.recommended,
  ...tseslint.configs.recommended,
  {
    files: ["app/anamnesis/**/*.ts", "packages/*/src/**/*.ts"],
    plugins: { sonarjs },
    rules: {
      complexity: ["error", { max: 21 }],
      "sonarjs/cognitive-complexity": ["error", 21],
      "@typescript-eslint/no-explicit-any": "error",
      "@typescript-eslint/no-unused-vars": ["error", { argsIgnorePattern: "^_", varsIgnorePattern: "^_", caughtErrors: "none", ignoreRestSiblings: true }],
      "@typescript-eslint/no-this-alias": ["error", { allowedNames: ["runtime"] }],
      "no-restricted-imports": ["error", { patterns: [
        { group: ["**/packages/*/src/*", "**/protocol/src/*", "**/core/src/*"], message: "Import the package entry (@anamnesis/protocol, @anamnesis/core) instead of a deep relative path." },
      ] }],
    },
  },
);
