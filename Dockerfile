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
