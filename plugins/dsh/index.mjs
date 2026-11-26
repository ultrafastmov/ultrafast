export const name = 'ultrafast';
export const inject = ['mcpManager'];

/** Runs `uf mcp` (stdio) for as long as the plugin is enabled. UF_BIN overrides the uf on PATH. */
export function apply(ctx) {
  ctx.mcpManager.register('ultrafast', {
    transport: 'stdio',
    command: process.env.UF_BIN || 'uf',
    args: ['mcp'],
    startupTimeoutMs: 30000,
    toolCallTimeoutMs: 600000,
  });
}
