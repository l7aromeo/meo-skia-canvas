// Flat config. Prettier owns layout, so nothing here is about formatting --
// these rules are the ones a formatter cannot answer: a name that is never
// read, a promise nobody waits for, a `case` that falls into the next.
//
// `tsconfig.json` deliberately typechecks only the two declaration files, so
// this is the only static analysis the shipped runtime in `lib/` gets.
import js from "@eslint/js";
import globals from "globals";

export default [
  {
    ignores: [
      "vendor/",
      "target/",
      "node_modules/",
      "lib/skia.node",
      "scripts/typedoc/node_modules/",
      // Generated from lib/*.d.ts by TypeDoc; not ours to lint.
      "target/jsdoc/",
      // Scratch, both of them: `.test-sandbox` is what the suite writes per
      // run and `.gitnexus` is tool residue. Git ignores both, ESLint does
      // not read .gitignore, and between them they were two thirds of the
      // first run's findings.
      ".test-sandbox/",
      ".gitnexus/",
    ],
  },

  js.configs.recommended,

  {
    files: ["**/*.js", "**/*.mjs"],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: "commonjs",
      globals: { ...globals.node },
    },
    rules: {
      // Arguments are not checked. The accessors in `lib/classes` forward
      // through `this.ƒ("name", ...arguments)`, so their named parameters are
      // never read -- they are the signature the Canvas API documents, and
      // deleting them would delete the documentation. That idiom accounts for
      // 166 of the 187 this rule first reported. Unused *variables* are still
      // errors, which is the half that finds leftovers.
      "no-unused-vars": ["error", { args: "none", varsIgnorePattern: "^_" }],

    },
  },

  {
    // The browser build and the visual tests run in a page, not in Node.
    files: ["lib/browser.js", "tests/visual/**/*.js"],
    languageOptions: { globals: { ...globals.browser } },
  },

  {
    files: ["**/*.mjs", "scripts/**/*.mjs", "lib/*.mjs"],
    languageOptions: { sourceType: "module" },
  },

  {
    files: ["tests/**/*.js", "tests/**/*.mjs"],
    languageOptions: { globals: { ...globals.node } },
  },
];
