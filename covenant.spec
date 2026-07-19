# HEAD-only spec: builds the checked-out tree via rpmbuild --build-in-place.
# No Source0 archive, no checksums, no signatures. Release binding (immutable
# archive URL, sha256, repository hosting, signing) is operator-owned, matching
# Formula/covenant.rb and flake.nix.
Name:           covenant
Version:        0.1.0~head
Release:        1%{?dist}
Summary:        Local control plane for governed autonomous agents
License:        Apache-2.0
URL:            https://opencovenant.org
ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  pkg-config
BuildRequires:  pkgconfig(openssl)
BuildRequires:  systemd-rpm-macros

%description
Covenant runs covenantd, a local daemon that gives autonomous agents
governance, permissions, accountability, and a tamper-proof audit record,
plus the covenant operator CLI.

HEAD-only package: built from the working tree, not a tagged release.
Release binding (immutable archives, checksums, signatures, repository
hosting) is operator-owned.

%build
cd agent-os
cargo build -p covenantd -p covenant --locked --release

%install
install -D -m 0755 agent-os/target/release/covenantd %{buildroot}%{_bindir}/covenantd
install -D -m 0755 agent-os/target/release/covenant %{buildroot}%{_bindir}/covenant
install -d %{buildroot}%{_unitdir}
# Same unit contract as debian/covenant.covenantd.service and the flake's
# nixosModules.covenant: COVENANT_HOME pinned to the managed state directory.
cat > %{buildroot}%{_unitdir}/covenantd.service <<'EOF'
[Unit]
Description=Covenant local control plane daemon
After=network.target

[Service]
ExecStart=/usr/bin/covenantd
Restart=always
Environment=COVENANT_HOME=/var/lib/covenant
StateDirectory=covenant

[Install]
WantedBy=multi-user.target
EOF

%post
%systemd_post covenantd.service

%preun
%systemd_preun covenantd.service

%postun
%systemd_postun_with_restart covenantd.service

%files
%license LICENSE
%{_bindir}/covenantd
%{_bindir}/covenant
%{_unitdir}/covenantd.service

%changelog
* Sun Jul 19 2026 Covenant <covenant@users.noreply.github.com> - 0.1.0~head-1
- HEAD-only packaging template building covenantd and covenant from the
  agent-os workspace with cargo --locked --release; systemd unit mirrors
  the Nix module contract (COVENANT_HOME=/var/lib/covenant).
