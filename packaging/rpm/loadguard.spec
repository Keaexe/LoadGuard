# Packages release binaries built beforehand. Build with:
#   rpmbuild -bb loadguard.spec --define "_sourcedir <dir>" --define "ver <version>"
# where <dir> holds bin/ (the binaries) and a copy of the source tree in src/.
Name:           loadguard
Version:        %{ver}
Release:        1%{?dist}
Summary:        Limit the bandwidth of running apps
License:        GPL-3.0-only
URL:            https://github.com/Keaexe/LoadGuard
ExclusiveArch:  x86_64 aarch64
BuildRequires:  systemd-rpm-macros
Requires:       nftables
Requires:       polkit
Requires:       systemd

%global debug_package %{nil}

%description
LoadGuard sets download and upload limits for individual apps, including
ones already running, from a window, the system tray or the command line.
A small system service applies the limits with nftables.

%install
%{_sourcedir}/src/packaging/install-files.sh %{_sourcedir}/bin %{buildroot} %{_libexecdir}/loadguard
install -Dm644 %{_sourcedir}/src/LICENSE %{buildroot}%{_datadir}/licenses/%{name}/LICENSE

%post
%systemd_post loadguard.service
if [ -d /run/systemd/system ]; then
    busctl call org.freedesktop.DBus /org/freedesktop/DBus org.freedesktop.DBus \
        ReloadConfig >/dev/null 2>&1 || :
fi

%preun
%systemd_preun loadguard.service

%postun
%systemd_postun_with_restart loadguard.service

%files
%license %{_datadir}/licenses/%{name}/LICENSE
%{_bindir}/loadguard
%{_bindir}/loadguard-gui
%{_libexecdir}/loadguard/
%{_unitdir}/loadguard.service
%{_presetdir}/80-loadguard.preset
%{_datadir}/dbus-1/system-services/io.github.keaexe.LoadGuard1.service
%{_datadir}/dbus-1/system.d/io.github.keaexe.LoadGuard1.conf
%{_datadir}/polkit-1/actions/io.github.keaexe.LoadGuard.policy
%{_datadir}/applications/io.github.keaexe.LoadGuard.desktop
%{_datadir}/icons/hicolor/scalable/apps/io.github.keaexe.LoadGuard.svg
