import { describe, expect, it } from "vitest";

import { typedPath, withFilePaths } from "./terminalText";

describe("the terminal's typed text", () => {
  it("types an image bare, as agents take it, and quotes anything else", () => {
    expect(typedPath({ path: "/home/me/.muxflow/uploads/shot.png", name: "shot.png", mediaType: "image/png" })).toBe("/home/me/.muxflow/uploads/shot.png");
    expect(typedPath({ path: "/home/me/my shot.png", name: "my shot.png", mediaType: "image/png" })).toBe("'/home/me/my shot.png'");
    expect(typedPath({ path: "/tmp/notes.txt", name: "notes.txt", mediaType: "text/plain" })).toBe("'/tmp/notes.txt'");
    expect(typedPath({ path: "/tmp/it's.txt", name: "it's.txt", mediaType: "text/plain" })).toBe(`'/tmp/it'"'"'s.txt'`);
    expect(typedPath({ path: "-rf", name: "-rf", mediaType: "text/plain" })).toBe("'./-rf'");
  });

  it("puts the paths after the text", () => {
    const shot = { path: "/u/shot.png", name: "shot.png", mediaType: "image/png" };
    expect(withFilePaths("what is this? ", [shot])).toBe("what is this? /u/shot.png");
    expect(withFilePaths("", [shot])).toBe("/u/shot.png");
  });

  it("types the text as it is when there are no files: a leading space keeps a command out of shell history", () => {
    expect(withFilePaths(" ls ", [])).toBe(" ls ");
  });
});
