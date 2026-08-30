use nudox_id::{ContentAuthority, ContentId, DependencySetDomain, ObjectDomain};

fn cross_domain(authority: ContentAuthority<DependencySetDomain>) {
    let _: ContentId<ObjectDomain> = authority.bind([0_u8; 31]);
}

fn main() {}
