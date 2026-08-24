'use client';

import { useState } from 'react';
import { useRouter } from 'next/navigation';

export function JobLookup() {
  const router = useRouter();
  const [jobId, setJobId] = useState('');

  return (
    <form
      id="job-lookup"
      className="job-lookup"
      onSubmit={(event) => {
        event.preventDefault();
        if (jobId.trim()) router.push(`/jobs/${encodeURIComponent(jobId.trim())}`);
      }}
    >
      <label htmlFor="job-id">Already have a job? View its public record</label>
      <div>
        <input
          id="job-id"
          value={jobId}
          onChange={(event) => setJobId(event.target.value)}
          placeholder="Job ID"
          required
        />
        <button type="submit">View job</button>
      </div>
    </form>
  );
}
