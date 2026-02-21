The Development Environment (Nix & Lix)
---------------------------------------

We rely on **Nix** to ensure that things stay isolated, and that bugs don't
become machine-specific. For this, use **Lix**, a modern, implementation of the
Nix package manager, to manage these environments.

#### 1. Installation

We don't manually install compilers, runtimes, or libraries. We describe them
in Nix and let Lix handle the isolation.

1.  **Install Lix**: Follow the [Lix installation guide]. Run the installer script:

    ~~~~ bash
    curl -sSfL https://install.lix.systems/lix | sh -s -- install
    ~~~~
2.  **Enable Flakes**: During installation, you will be prompted to enable
    **Flakes** and the **New CLI**. **Say yes.** Flakes provide pinning of our
    dependencies so the tooling can be rebuilt at any point in time.
3.  **Verify**: Ensure the binary is in your path by checking the version:

    ~~~~ bash
    nix --version
    ~~~~

#### 2. The Repository Entry Point: `nix shell`

Each NuDox repository contains a `flake.nix` file. This is the blueprint for
the project's environment. Instead of polluting your global system path with
specific versions of Node, Go, or Rust, we use ephemeral shells.

**To enter a development environment:**

1.  Navigate to any NuDox repository.
2.  Execute the entry command:

    ~~~~ bash
    nix shell
    ~~~~

This command pulls the exact dependencies defined in the flake, builds them (or
fetches them from a cache), and drops you into a shell where all required
tooling is available in your `$PATH`. When you exit the shell, your system
remains clean.

#### 3. Automation with `direnv`

Running `nix shell` manually every time you `cd` into a directory is tedious
and error-prone. We recommend using `direnv` to automate the loading of our Nix
environments.

1.  **Install direnv**: https://direnv.net/docs/installation.html.
2.  **Hook it**: Add `eval "$(direnv hook bash)"` (or your preferred shell) to your
    `~/.bashrc`.
3.  **Allow it**: Run `direnv allow` inside a repository.

From then on, the environment will load and unload automatically as you move in
and out of project directories.

#### 4. Troubleshooting: “The Pure Context”

 -  **Binary Blobs**: If a tool you installed via your OS package manager (like
    `apt` or `brew`) is missing inside a `nix shell`, this is by design. Nix
    environments are meant to be pure. If a tool is missing, add it to the
    repository's `flake.nix` rather than installing it globally.
 -  **Permissions**: If Nix complains about `trusted-users`, you may need to add
    your user to `/etc/nix/nix.conf`.

[Lix installation guide]: https://lix.systems/install/


Git
---

### Commit Standards

For consistency, every commit should follow the [conventional commit]
standards. Read it if you like, but frankly it just means that your commit
messages need to follow this format (Add it to your gitconfig if you'd like):

~~~~ gitcommit
<type>(<optional scope>): <subject>

<optional body>

# Types: build (deps/build), chore (maintenance), ci, docs, feat (new),
#        fix (bug), perf, refactor (no behavior change), revert (undo),
#        style (format/comments), test 
# Scope: from edited filenames.
# Body: bullets for what + why.
# Footer: Fixes: | BREAKING CHANGE: | Refs: | Co-authored-by:
~~~~

In your PR's and anything else, there's no expectation, it's just important for
browsing the log or quickly finding out the history of a file. We also use it
for changelog generation, so no one has to worry about presentation there.

[conventional commit]: https://www.conventionalcommits.org/en/v1.0.0/

### Radicle

We use [Radicle] for hosting our private repositories. Unlike centralized
forges, Radicle is a peer-to-peer protocol where your identity is tied to
cryptographic keys rather than an email address.

#### 1. Identity & Node Setup

Before you can interact with the network, you must forge your identity.

1.  **Installation**: [Install Radicle] for your OS.
2.  **Authentication**: Run `rad auth` in your terminal. You will be prompted for
    an alias and a passphrase.
     -  **Note**: Your passphrase encrypts your private key. If you lose it, you lose
        access to your identity and your ability to sign code. There is no
        password reset :(.
3.  **Identify your DID**: Upon completion, the CLI returns your **DID**
    (Decentralized Identifier). You can view this at any time by running
    `rad self --did`.
4.  **Start the Engine**: Radicle requires a local node to handle replication and
    gossip. Start it as a background daemon:

    ~~~~ bash
    rad node start
    ~~~~

#### 2. Connecting to the Seed Node

To bridge into our private network, you need to connect to our primary seed
node (the “leaf” node). This node acts as a 24/7 relay for our private
repositories.

Connect to the remote peer:

~~~~ bash
rad node connect z6MkmTC76GDv4H7YdZB9UvMhjxpxZXoNTeQaMqGsoiRpZsJf@100.114.38.65:8776
~~~~

#### 3. Provisioning Permissions

Because we operate with private repositories, visibility is restricted to an
explicit **allow list**. You must add your new DID to the repository’s identity
on the server.

1.  **Access the Server**: SSH into the hosting seed:

    ~~~~ bash
    ssh leaf@100.114.38.65
    ~~~~
2.  **Locate the Project**: Navigate to the specific repository directory in the
    seed's storage.
3.  **Update the Allow List**: Run the following to grant your identity access:

    ~~~~ bash
    rad id update --allow <YOUR_DID>
    ~~~~

    *This requires a delegate passphrase to confirm the change to the repository's
    canonical state.*

#### 4. Verification & Workflow

Once permissions are set, verify that your local node has a healthy connection
to the seed. Run `rad node` and look for the `100.114.38.65` record; a **green
checkmark** indicates a successful gossip connection.

**Collaboration via Desktop or CLI:**

 -  **GUI**: Use the [Radicle Desktop App]. It will automatically detect
    repositories you have permission to seed.
 -  **CLI**: Use `rad clone <RID>` to pull a local working copy.
 -  **Social Artifacts**: We use **Patches** for code review (the Radicle
    equivalent of a PR) and **Issues** for bug tracking.
 -  **Automation**: Check the `justfile` in each repository for common recipes
    (e.g., viewing pending patches or running tests offline).

Everything in Radicle is **local-first**. You can commit, open issues, and
iterate on patches while offline; your node will automatically synchronize your
changes with the rest of the network the moment you reconnect.

#### Maintaining Synchronicity

Radicle is local-first, meaning your node is its own source of truth. To ensure
the hosting server (the leaf node) and your teammates see your work:

1.  **Announce your changes**: After a `git push rad`, your work is only on your
    machine. Force the network to notice by running:

    ~~~~ bash
    rad sync
    ~~~~
2.  **Verify replication**: If you aren't sure if the server has your latest
    commit, check the sync status:

    ~~~~ bash
    rad sync status
    ~~~~
3.  **Watch the repository**: If you’ve just been added to a new private repo, you
    must explicitly tell your node to start tracking it:

    ~~~~ bash
    rad seed rad:z3...your_repo_id_here
    ~~~~

[Radicle]: https://radicle.xyz/
[Install Radicle]: https://radicle.xyz/download
[Radicle Desktop App]: https://desktop.radicle.xyz/

### Repository Hygiene: The `.gitignore` Allowlist

To maintain a pure state across the repositories, we utilizes an **allowlist
pattern** for Git tracking. Unlike standard repositories where you ignore
specific “trash,” we ignore **everything** by default and explicitly allow only
what is necessary. Hopefully, this forces us to be intentional about how we
structure things, and the dependencies we bring in.

For most files, it's nothing to think about, but for changes in structure (or
the addition of anything top-level), you'll most likely have to change some
entries.

**Parent Directory Blobs**: If you find that an allowed file is still being
ignored, ensure a parent directory isn't explicitly `deny`‘d in the script.
Git's performance optimizations prevent it from looking inside a denied folder,
even if a sub-file is “allowed.”
