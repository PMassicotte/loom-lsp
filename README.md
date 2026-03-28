# Loom

[![Build](https://github.com/PMassicotte/loom/actions/workflows/rust.yml/badge.svg)](https://github.com/PMassicotte/loom/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-2024_edition-orange.svg)](https://www.rust-lang.org)
[![Status](https://img.shields.io/badge/status-experimental-red.svg)]()

Write [Quarto](https://quarto.org/) documents and get full IDE support for every language in your notebook at the same time.

## The problem

Quarto `.qmd` files are powerful: Python, R, markdown, and YAML all in one document. But your editor only understands one language at a time. You get autocomplete for Python _or_ R, never both. Diagnostics miss errors across languages. Hover docs disappear the moment you cross a code fence.

## What Loom does

Loom is a language server that sits between your editor and your existing language tools. It understands the structure of a Quarto document and routes each part to the right server. For example, Python chunks to pyright, R chunks to the R language server, markdown to marksman. Your editor talks to one server; Loom handles the rest.

The result: full autocomplete, diagnostics, hover documentation, and go-to-definition across your entire document, for every language, simultaneously.

## Works with your existing tools

Loom doesn't replace pyright or the R language server, it connects them. If you've already configured your Python or R environment, Loom picks it up automatically. No new tooling to learn, no duplicate configuration.

## Any editor, any workflow

Loom speaks standard LSP, so it works with Neovim, VS Code, Emacs, or any editor with LSP support. If your editor can run a language server, it can run Loom.
