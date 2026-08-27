// next/font/google runs in the Next build pipeline and cannot resolve under
// vitest. Loaders return the same shape, so a stub keeps layout importable.
type Loader = (options: { variable?: string }) => {
  variable: string;
  className: string;
  style: { fontFamily: string };
};

const loader: Loader = ({ variable = '--font-stub' } = { variable: '--font-stub' }) => ({
  variable: variable.replace(/^--/, 'font-'),
  className: 'font-stub',
  style: { fontFamily: 'stub' },
});

export const Archivo = loader;
export const JetBrains_Mono = loader;
