set -eu
unshare -n -- sleep 300 & echo $! > "/home/Qui/.t3/worktrees/acos/t3-a531561b-3a8f5c6b7589fc68/results/handoff/NET3/run/pasta-T/ns/a.pid"
unshare -n -- sleep 300 & echo $! > "/home/Qui/.t3/worktrees/acos/t3-a531561b-3a8f5c6b7589fc68/results/handoff/NET3/run/pasta-T/ns/b.pid"
sleep 0.3
A=$(cat "/home/Qui/.t3/worktrees/acos/t3-a531561b-3a8f5c6b7589fc68/results/handoff/NET3/run/pasta-T/ns/a.pid"); B=$(cat "/home/Qui/.t3/worktrees/acos/t3-a531561b-3a8f5c6b7589fc68/results/handoff/NET3/run/pasta-T/ns/b.pid")
# The pair is created *inside* A: a veth cannot be created in the host's netns
# from an unprivileged user namespace (that netns is not ours to administer).
nsenter -t $A -n -- ip link add vrel type veth peer name vsess
nsenter -t $A -n -- ip link set vsess netns /proc/$B/ns/net
nsenter -t $A -n -- ip addr add 10.163.1.1/30 dev vrel
nsenter -t $A -n -- ip link set vrel up
nsenter -t $A -n -- ip link set lo up
nsenter -t $B -n -- ip addr add 10.163.1.2/30 dev vsess
nsenter -t $B -n -- ip link set vsess up
nsenter -t $B -n -- ip link set lo up
nsenter -t $B -n -- ip route add default via 10.163.1.1
# A forwards to its own pasta interface (the relay netns's job).
nsenter -t $A -n -- sysctl -qw net.ipv4.ip_forward=1
