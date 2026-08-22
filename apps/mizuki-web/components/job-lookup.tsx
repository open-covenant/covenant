'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';

export function JobLookup() {
  const router = useRouter();
  const [jobId, setJobId] = useState('');

  return (
    <form
      className="job-lookup"
      onSubmit={(event) => {
        event.preventDefault();
        if (jobId.trim()) router.push(`/jobs/${encodeURIComponent(jobId.trim())}`);
      }}
    >
      <label htmlFor="job-id">Already paid? Open a job receipt</label>
      <div>
        <input
          id="job-id"
          value={jobId}
          onChange={(event) => setJobId(event.target.value)}
          placeholder="job identifier"
          required
        />
        <button type="submit">Open receipt ↗</button>
      </div>
    </form>
  );
}
