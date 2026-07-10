import { cac } from "cac";
import { cancel, intro, isCancel, log, outro, select } from "@clack/prompts";
import { packageLinux } from "./targets/linux";

const TARGETS = ["linux", "windows", "macos"] as const;
type Target = (typeof TARGETS)[number];

function isTarget(value: string): value is Target {
  return (TARGETS as readonly string[]).includes(value);
}

async function run(target: string | undefined): Promise<number> {
  intro("Saffron Anima · packager");

  let selected = target;
  if (!selected) {
    const choice = await select({
      message: "Package for which target?",
      options: TARGETS.map((value) => ({ value, label: value })),
    });
    if (isCancel(choice)) {
      cancel("Cancelled.");
      return 1;
    }
    selected = choice;
  }

  if (!isTarget(selected)) {
    cancel(`Unknown target '${selected}' (expected: ${TARGETS.join(", ")}).`);
    return 2;
  }

  if (selected !== "linux") {
    log.warn(`Packaging for ${selected} is not yet implemented.`);
    outro("Nothing to do.");
    return 1;
  }

  try {
    await packageLinux();
    outro("Done.");
    return 0;
  } catch (error) {
    const stderr = (error as { stderr?: { toString(): string } })?.stderr?.toString().trim();
    log.error(stderr || (error instanceof Error ? error.message : String(error)));
    cancel("Packaging failed.");
    return 1;
  }
}

const cli = cac("packager");
cli
  .command("[target]", "Package the editor as a distributable (linux | windows | macos)")
  .action(async (target?: string) => process.exit(await run(target)));
cli.help();
cli.parse();
