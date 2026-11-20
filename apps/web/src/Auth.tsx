import { useMutation } from "@tanstack/react-query";
import { KeyRound, LockKeyhole } from "lucide-react";
import { api, cache } from "./api";
import type { Session } from "./model";
import { Brand, Field, Form, str } from "./ui";

export function Auth({ setup }: { setup: boolean }) {
  const login = useMutation({
    mutationFn: (d: FormData) => {
      if (setup && str(d, "confirm") !== str(d, "password"))
        throw new Error("Passwords do not match.");
      return api<Session>(setup ? "setup" : "login", "POST", {
        username: str(d, "username"),
        password: str(d, "password"),
        ...(setup ? { bootstrapSecret: str(d, "secret") } : {}),
      });
    },
    onSuccess: (s) => {
      cache.setQueryData(["session"], s);
    },
  });
  return (
    <main className="auth-shell">
      <section className="auth-brand">
        <Brand />
        <div>
          <div className="auth-art" aria-hidden="true" />
          <h1>
            Manifests in.
            <br />
            <span>Mirrors out.</span>
          </h1>
        </div>
        <p>Self-hosted artifact mirroring</p>
      </section>
      <section className="auth-form">
        <div className="auth-icon">
          {setup ? <KeyRound /> : <LockKeyhole />}
        </div>
        <h2>{setup ? "Set up Metiva" : "Sign in"}</h2>
        <p>
          {setup
            ? "Create the administrator account for this instance."
            : "Sign in to your mirror workspace."}
        </p>
        <Form
          submit={setup ? "Create administrator" : "Sign in"}
          onSubmit={login.mutate}
          error={login.error}
          busy={login.isPending}
        >
          {setup && (
            <Field
              label="Bootstrap secret"
              hint="Read bootstrap-secret from your instance’s data directory."
            >
              <input
                name="secret"
                type="password"
                required
                autoComplete="off"
                autoFocus
              />
            </Field>
          )}
          <Field label="Username">
            <input
              name="username"
              autoComplete="username"
              autoFocus={!setup}
              maxLength={64}
              required
            />
          </Field>
          <Field
            label="Password"
            hint={setup ? "Use at least 12 characters." : undefined}
          >
            <input
              name="password"
              type="password"
              autoComplete={setup ? "new-password" : "current-password"}
              minLength={setup ? 12 : undefined}
              maxLength={256}
              required
            />
          </Field>
          {setup && (
            <Field label="Confirm password">
              <input
                name="confirm"
                type="password"
                autoComplete="new-password"
                required
              />
            </Field>
          )}
        </Form>
      </section>
    </main>
  );
}
