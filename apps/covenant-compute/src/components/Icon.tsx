import type { ReactNode, SVGProps } from 'react';

export type IconName =
  | 'arrow'
  | 'chat'
  | 'check'
  | 'clock'
  | 'copy'
  | 'external'
  | 'gpu'
  | 'image'
  | 'receipt'
  | 'refresh'
  | 'server'
  | 'shield'
  | 'stop'
  | 'wallet'
  | 'workspace';

const paths: Record<IconName, ReactNode> = {
  arrow: <path d="M5 12h14m-5-5 5 5-5 5" />,
  chat: (
    <>
      <path d="M5 6.5A2.5 2.5 0 0 1 7.5 4h9A2.5 2.5 0 0 1 19 6.5v6a2.5 2.5 0 0 1-2.5 2.5H11l-4.5 4v-4A2.5 2.5 0 0 1 4 12.5v-6Z" />
      <path d="M8 8.5h8M8 11.5h5" />
    </>
  ),
  check: <path d="m5 12 4.2 4.2L19 6.5" />,
  clock: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <path d="M12 7.5V12l3 2" />
    </>
  ),
  copy: (
    <>
      <rect x="8" y="8" width="10" height="11" rx="2" />
      <path d="M16 8V7a2 2 0 0 0-2-2H7a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h1" />
    </>
  ),
  external: <path d="M14 5h5v5M19 5l-8 8M18 13v4a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4" />,
  gpu: (
    <>
      <rect x="5" y="6" width="14" height="12" rx="2" />
      <circle cx="12" cy="12" r="3.5" />
      <path d="M2.5 9H5M2.5 15H5M19 10h2.5M19 14h2.5" />
    </>
  ),
  image: (
    <>
      <rect x="4" y="5" width="16" height="14" rx="2" />
      <circle cx="9" cy="10" r="1.5" />
      <path d="m5 17 4.5-4 3 2.5 2.5-2 4 3.5" />
    </>
  ),
  receipt: (
    <>
      <path d="M7 3.5 9 5l3-1.5L15 5l2-1.5V20l-2-1.5L12 20l-3-1.5L7 20V3.5Z" />
      <path d="M9.5 9h5M9.5 12h5M9.5 15h3" />
    </>
  ),
  refresh: (
    <>
      <path d="M18.5 8A7.5 7.5 0 0 0 5.3 6.2L3.5 8" />
      <path d="M3.5 4.5V8h3.7M5.5 16A7.5 7.5 0 0 0 18.7 17.8l1.8-1.8" />
      <path d="M20.5 19.5V16h-3.7" />
    </>
  ),
  server: (
    <>
      <rect x="4" y="5" width="16" height="6" rx="2" />
      <rect x="4" y="13" width="16" height="6" rx="2" />
      <path d="M8 8h.01M8 16h.01M12 8h5M12 16h5" />
    </>
  ),
  shield: (
    <>
      <path d="M12 3.5 19 6v5.5c0 4.2-2.9 7.5-7 9-4.1-1.5-7-4.8-7-9V6l7-2.5Z" />
      <path d="m8.7 12 2.1 2.1 4.5-4.7" />
    </>
  ),
  stop: (
    <>
      <circle cx="12" cy="12" r="8.5" />
      <rect x="9" y="9" width="6" height="6" rx="1" />
    </>
  ),
  wallet: (
    <>
      <path d="M5 7.5V6a2 2 0 0 1 2-2h10v3.5" />
      <rect x="4" y="7.5" width="16" height="12" rx="2" />
      <path d="M15 11h5v5h-5a2.5 2.5 0 0 1 0-5Z" />
    </>
  ),
  workspace: (
    <>
      <rect x="3.5" y="4.5" width="17" height="13" rx="2" />
      <path d="m7.5 9 2.5 2.5L7.5 14M12 14h4.5M9 20h6" />
    </>
  ),
};

interface IconProps extends SVGProps<SVGSVGElement> {
  name: IconName;
}

export function Icon({ name, ...props }: IconProps) {
  return (
    <svg
      aria-hidden="true"
      fill="none"
      viewBox="0 0 24 24"
      stroke="currentColor"
      strokeLinecap="round"
      strokeLinejoin="round"
      strokeWidth="1.7"
      {...props}
    >
      {paths[name]}
    </svg>
  );
}
