# Use the latest Fedora Rawhide image for development
FROM fedora:rawhide

# Install Git and minimize the image size. Everything past `rust` is what the
# integration test needs, named explicitly so the image cannot drift out from
# under it if the base image stops pulling one in: systemd-container provides
# systemd-nspawn/machinectl for the fixture machines it boots, systemd-networkd
# the networkd collector's source, and shadow-utils the useradd its capability
# matrix runs monitord with (see REQUIRED_CONTAINER_TOOLS).
RUN dnf install -y cargo git rust systemd systemd-container systemd-networkd shadow-utils && \
    dnf clean all && \
    rm -rf /var/cache/dnf

# Verify Git, Rust, and Cargo installation
RUN git --version
RUN rustc --version
RUN cargo --version

# Drop systemd-networkd config files for a dummy interface used in integration tests.
# The .netdev creates the virtual device, the .link sets a description, and the
# .network file causes networkd to manage it (no IP — it will stay in carrier/down state).
RUN printf '%s\n' '[NetDev]' 'Name=monitord0' 'Kind=dummy' \
      > /etc/systemd/network/10-monitord-dummy.netdev && \
    printf '%s\n' '[Match]' 'OriginalName=monitord0' '' '[Link]' \
      'Description=Monitord integration-test dummy interface' \
      > /etc/systemd/network/10-monitord-dummy.link && \
    printf '%s\n' '[Match]' 'Name=monitord0' '' '[Network]' \
      > /etc/systemd/network/10-monitord-dummy.network
