const steps = [
  {
    number: '01',
    label: 'Authorized paid job',
    detail: 'A maintainer authorizes one public issue and pays the fixed quote.',
  },
  {
    number: '02',
    label: 'Full USDC refund',
    detail: 'If Mizuki cannot deliver, the quoted USDC payment returns to the original payer.',
  },
  {
    number: '03',
    label: 'Funded maintenance bounty',
    detail: 'Separate SOL escrow must finalize before the bounty is published.',
  },
  {
    number: '04',
    label: 'Verified improvement evidence',
    detail: 'A reviewed, merged fix can support a future capability update.',
  },
];

export function Transformation() {
  return (
    <ol className="transformation" aria-label="What happens after a paid job cannot be delivered">
      {steps.map((step, index) => (
        <li key={step.number}>
          <span className="transformation-number">{step.number}</span>
          <div>
            <strong>{step.label}</strong>
            <p>{step.detail}</p>
          </div>
          {index < steps.length - 1 && (
            <span className="transformation-arrow" aria-hidden="true">
              →
            </span>
          )}
        </li>
      ))}
    </ol>
  );
}
