import { InvokeError, invoke } from "../../shell";
import type { CommandParamsMap, CommandResultMap, ControlFailureDto } from "../../protocol";

type CommandName = keyof CommandParamsMap & keyof CommandResultMap;
type EmptyCommandName = {
  [C in CommandName]: keyof CommandParamsMap[C] extends never ? C : never;
}[CommandName];

/// A rejected control call carrying the exact shared failure object.
export class ControlError extends Error {
  readonly failure: ControlFailureDto;
  constructor(failure: ControlFailureDto) {
    super(failure.message);
    this.name = "ControlError";
    this.failure = failure;
  }

  get code(): ControlFailureDto["code"] {
    return this.failure.code;
  }

  get diagnostic(): Extract<ControlFailureDto, { code: "diagnostic" }>["diagnostic"] | null {
    return this.failure.code === "diagnostic" ? this.failure.diagnostic : null;
  }
}

/// True when a call was discarded because a project load is in flight. Background poll lanes drop
/// these silently and retry once the load settles.
export function isBusyLoading(err: unknown): boolean {
  return err instanceof ControlError && err.code === "busy-loading";
}

function toControlError(raw: unknown): ControlError {
  if (raw instanceof ControlError) {
    return raw;
  }
  if (raw instanceof InvokeError) {
    return new ControlError(raw.failure);
  }
  return new ControlError({
    code: "bridge",
    message: raw instanceof Error ? raw.message : String(raw),
  });
}

export function call<C extends EmptyCommandName>(cmd: C): Promise<CommandResultMap[C]>;
export function call<C extends CommandName>(
  cmd: C,
  params: CommandParamsMap[C],
): Promise<CommandResultMap[C]>;
/// The one engine call path: the generic Rust passthrough `invoke('control', …)`. Rust already
/// turns an engine `ok:false` into a rejection, so this only narrows the resolve type and
/// normalizes the rejection into a [`ControlError`].
export async function call<C extends CommandName>(
  cmd: C,
  params?: CommandParamsMap[C],
): Promise<CommandResultMap[C]> {
  try {
    return await invoke<CommandResultMap[C]>("control", { cmd, params: params ?? {} });
  } catch (raw) {
    throw toControlError(raw);
  }
}
