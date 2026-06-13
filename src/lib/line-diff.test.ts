import { describe, expect, it } from "vitest"

import { diffLines } from "./line-diff"

describe("diffLines", () => {
  it("marks every line as context when unchanged", () => {
    expect(diffLines("a\nb\nc", "a\nb\nc")).toEqual([
      { type: "context", text: "a" },
      { type: "context", text: "b" },
      { type: "context", text: "c" },
    ])
  })

  it("treats an empty old text as all additions", () => {
    expect(diffLines("", "x\ny")).toEqual([
      { type: "add", text: "x" },
      { type: "add", text: "y" },
    ])
  })

  it("treats an empty new text as all deletions", () => {
    expect(diffLines("x\ny", "")).toEqual([
      { type: "del", text: "x" },
      { type: "del", text: "y" },
    ])
  })

  it("returns deletions before additions at a replacement", () => {
    expect(diffLines("a", "b")).toEqual([
      { type: "del", text: "a" },
      { type: "add", text: "b" },
    ])
  })

  it("keeps surrounding context around a middle change", () => {
    expect(diffLines("x\na\ny", "x\nb\ny")).toEqual([
      { type: "context", text: "x" },
      { type: "del", text: "a" },
      { type: "add", text: "b" },
      { type: "context", text: "y" },
    ])
  })

  it("detects a pure insertion", () => {
    expect(diffLines("a\nc", "a\nb\nc")).toEqual([
      { type: "context", text: "a" },
      { type: "add", text: "b" },
      { type: "context", text: "c" },
    ])
  })

  it("detects a pure deletion", () => {
    expect(diffLines("a\nb\nc", "a\nc")).toEqual([
      { type: "context", text: "a" },
      { type: "del", text: "b" },
      { type: "context", text: "c" },
    ])
  })

  it("ignores the empty line a trailing newline produces", () => {
    expect(diffLines("a\n", "a\n")).toEqual([{ type: "context", text: "a" }])
  })

  it("returns nothing for two empty texts", () => {
    expect(diffLines("", "")).toEqual([])
  })
})
