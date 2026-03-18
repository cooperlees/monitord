# Use the latest Fedora Rawhide image for development
FROM fedora:rawhide

# Install Git and minimize the image size
RUN dnf install -y cargo git rust systemd && \
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
