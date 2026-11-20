-- A failed child request has no response document. Freeze its outcome alongside
-- the root so recovery can distinguish an unavailable child from a missing snapshot.
CREATE TABLE nested_fetch_failures(
    root_observation INTEGER NOT NULL REFERENCES source_observations(id),
    request_url TEXT NOT NULL,
    error TEXT NOT NULL,
    PRIMARY KEY(root_observation,request_url)
);
