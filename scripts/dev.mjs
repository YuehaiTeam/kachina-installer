import { spawn } from 'node:child_process';

const rsbuild = spawn('pnpm', ['exec', 'rsbuild', 'dev'], {
  stdio: 'inherit',
  shell: true,
});

const cargo = spawn(
  'cargo',
  ['run', '--manifest-path', 'src-tauri/Cargo.toml'],
  {
    stdio: 'inherit',
    shell: true,
    env: { ...process.env, CARGO_TERM_COLOR: 'always' },
  },
);

const stop = (code = 0) => {
  rsbuild.kill();
  cargo.kill();
  process.exit(code ?? 0);
};

rsbuild.on('exit', (code) => {
  if (code) stop(code);
});
cargo.on('exit', (code) => stop(code ?? 0));
process.on('SIGINT', () => stop(0));
process.on('SIGTERM', () => stop(0));
