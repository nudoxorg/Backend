# Reproducible corpus declarations. Keep package sources and hashes in Nix.
# The flake consumes this directly; there is no second TOML manifest.
{
  packages = [
    {
      ecosystem = "crates.io";
      name = "bytes";
      versions = [
        {
          version = "1.11.0";
          hash = "1cww1ybcvisyj8pbzl4m36bni2jaz0narhczp1348gqbvkxh8lmk";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "hashbrown";
      versions = [
        {
          version = "0.17.1";
          hash = "0jmqz7i4yl6cm7rbn0i2ffkfrmwi6xkmzkaldr2v8bcsx2v0jngd";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "indexmap";
      versions = [
        {
          version = "2.13.0";
          hash = "05qh5c4h2hrnyypphxpwflk45syqbzvqsvvyxg43mp576w2ff53p";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "itoa";
      versions = [
        {
          version = "1.0.18";
          hash = "10jnd1vpfkb8kj38rlkn2a6k02afvj3qmw054dfpzagrpl6achlg";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "lazy_static";
      versions = [
        {
          version = "1.4.0";
          hash = "0in6ikhw8mgl33wjv6q6xfrb5b9jr16q8ygjy803fay4zcisvaz2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "libc";
      versions = [
        {
          version = "0.2.161";
          hash = "1lc5s3zd0491x9zxrv2kvclai1my1spz950pkkyry4vwh318k54f";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "log";
      versions = [
        {
          version = "0.4.17";
          hash = "0biqlaaw1lsr8bpnmbcc0fvgjj34yy79ghqzyi0ali7vgil2xcdb";
        }
        {
          version = "0.4.33";
          hash = "1bd9dmk22pxgnf0h0slba6rz99zb0a0b2mdhpk8p92bp26ycbvhc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "memchr";
      versions = [
        {
          version = "2.7.6";
          hash = "0wy29kf6pb4fbhfksjbs05jy2f32r2f3r1ga6qkmpz31k79h0azm";
        }
        {
          version = "2.8.0";
          hash = "0y9zzxcqxvdqg6wyag7vc3h0blhdn7hkq164bxyx2vph8zs5ijpq";
        }
        {
          version = "2.8.3";
          hash = "161xa63ipfanf8v3nb82xd5hqgydv55nzw59wyngqbz6alfaz2yg";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "nom";
      versions = [
        {
          version = "5.1.3";
          hash = "0jyxc4d3pih60pp8hvzpg5ajh16s273cpnsdpzp04qv7g8w9m588";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "once_cell";
      versions = [
        {
          version = "1.20.2";
          hash = "0xb7rw1aqr7pa4z3b00y7786gyf8awx2gca3md73afy76dzgwq8j";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "parking_lot";
      versions = [
        {
          version = "0.12.1";
          hash = "13r2xk7mnxfc5g0g6dkdxqdqad99j7s7z8zhzz4npw5r0g0v4hip";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "regex";
      versions = [
        {
          version = "1.10.3";
          hash = "05cvihqy0wgnh9i8a9y2n803n5azg2h0b7nlqy6rsvxhy00vwbdn";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde";
      versions = [
        {
          version = "1.0.196";
          hash = "0civrvhbwwk442xhlkfdkkdn478by486qxmackq6k3501zk2c047";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_json";
      versions = [
        {
          version = "1.0.113";
          hash = "0ycaiff7ar4qx5sy9kvi1kv9rnnfl15kcfmhxiiwknn3n5q1p039";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syn";
      versions = [
        {
          version = "1.0.109";
          hash = "0ds2if4600bd59wsv7jjgfkayfzy3hnazs394kz6zdkmna8l3dkj";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "thiserror";
      versions = [
        {
          version = "1.0.40";
          hash = "1b7bdhriasdsr99y39d50jz995xaz9sw3hsbb6z9kp6q9cqrm34p";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tracing";
      versions = [
        {
          version = "0.1.40";
          hash = "1vv48dac9zgj9650pg2b4d0j3w6f3x9gbggf43scq5hrlysklln3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "unicode-width";
      versions = [
        {
          version = "0.1.11";
          hash = "11ds4ydhg8g7l06rlmh712q41qsrd0j0h00n1jm74kww3kqk65z5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "clap";
      versions = [
        {
          version = "4.5.1";
          hash = "1ni08mammjr61fg7cx900zgvcdfb4z7fjrlm1xx5f4r9xx0xa669";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rayon";
      versions = [
        {
          version = "1.9.0";
          hash = "1gdk945j52vq3zx5vb4yzc3yyz19bf2vs8kh47pg7r46pk8kx5p4";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/pkg/errors";
      versions = [
        {
          version = "v0.8.1";
          hash = "1n2im6ss6ay33n5h6d7wm5h77brgalkzam74vskrcwqb6hhx0isf";
        }
        {
          version = "v0.9.1";
          hash = "01xxy95b9w8djvr9c1b701pbscdrac7mw88k7452j5h6rn5nphyl";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/google/uuid";
      versions = [
        {
          version = "v1.6.0";
          hash = "1vd6725h6mpakfx39k6x1lagbnqy8h34ws2rw812gx0pf8vjzw6h";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/spf13/cobra";
      versions = [
        {
          version = "v1.10.2";
          hash = "0bc74rggynmiipb619dqqdrin3pp5x2h91n9abg0y7k3rmpsw2m0";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/spf13/viper";
      versions = [
        {
          version = "v1.21.0";
          hash = "17i4gg6dmwrwaibwxv61s6q56c0rcxnxxzhdkz22fh5pvg6f95j0";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/stretchr/testify";
      versions = [
        {
          version = "v1.11.1";
          hash = "0vn9vwx8yv7x94556zb64cw9px0zhyyawclz5fvh8lxd3rb5ncmp";
        }
        {
          version = "v1.9.0";
          hash = "1q5qm6g276zpplnw6jbwa1rb3gczkwc8m46669a1p6v8rdrlypgf";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "gopkg.in/yaml.v3";
      versions = [
        {
          version = "v3.0.1";
          hash = "05d0m7qk217jw99jrxc9yiwhrj91iahsw77yda7a03ihwv2gpf5a";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gin-gonic/gin";
      versions = [
        {
          version = "v1.12.0";
          hash = "0gmnbbj638lk93y8jh6bh6vr0hks2z06ifxc4zas0cxi6a9zk35f";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "golang.org/x/sync";
      versions = [
        {
          version = "v0.22.0";
          hash = "1q5dky8a99cnz230ppa61jwljk4bv51mqxj33by7r1p7ihjpvijb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "golang.org/x/text";
      versions = [
        {
          version = "v0.40.0";
          hash = "1qwysq8sgwhhnvbbfzpvn2r9cqsy81iz3wy1hpfm43wdfa276x87";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/prometheus/client_golang";
      versions = [
        {
          version = "v1.24.1";
          hash = "1hvgm2ipsqnsgphv5fhb2c8wrzlqc5nhc8zd23s6r4yrjbijgdlh";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/redis/go-redis/v9";
      versions = [
        {
          version = "v9.22.0";
          hash = "07pvp0lb4fhcb54vy6674fgyn0a030hc95drvfks2fkcxky1ypzx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/hashicorp/go-multierror";
      versions = [
        {
          version = "v1.1.1";
          hash = "0300qcl0y92w3mi7zpgx2nzsw9zaz1biwc2skk3fmzaixr0xhb4p";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/BurntSushi/toml";
      versions = [
        {
          version = "v1.6.0";
          hash = "1n0z042sq4iga4ai6aihp2aw5h2anl3j8853pl4f3zjb7xn3vw01";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/mattn/go-sqlite3";
      versions = [
        {
          version = "v1.14.49";
          hash = "1qcc077q866k6xl6wda5ldcr3irqy9dnyi9r3n6j7vc08xj6xk2l";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/golang-jwt/jwt/v5";
      versions = [
        {
          version = "v5.3.1";
          hash = "1izyasha43zam7548h7ayxkmgbsj693van35wp74dirb9r4p96jw";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/google/go-cmp";
      versions = [
        {
          version = "v0.7.0";
          hash = "0fgmnffr2c3ngzr7pdh9n3q1i2jzl7sd387vhcvhwcicdw2cxab4";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/robfig/cron/v3";
      versions = [
        {
          version = "v3.0.1";
          hash = "0w3cx1mnf9yylm1b543bsgc0y5grwpm51k5qa6j342128934brpb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fsnotify/fsnotify";
      versions = [
        {
          version = "v1.10.1";
          hash = "05mh599dxlq231q1g7yzssx2djrlvbcvsrk00hpdl66mw0sa7imj";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/samber/lo";
      versions = [
        {
          version = "v1.53.0";
          hash = "0ygmw5zzz68j5yrdknxcxfh9ndljx8nw928gr8b6lwx1jh0cysa9";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "go.uber.org/zap";
      versions = [
        {
          version = "v1.28.0";
          hash = "174i31w6k9agpiyjbllzpwzpkb1qkkp3s3ikx7180x2kjxs2imj9";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lodash";
      versions = [
        {
          version = "4.17.21";
          hash = "017qragyfl5ifajdx48lvz46wr0jc1llikgvc2fhqakhwp4pl23a";
        }
        {
          version = "4.17.20";
          hash = "1qpjahj6j8l9sag75f9sxfl85r6ab8f7v8w5axv9319wzim8ranj";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "zod";
      versions = [
        {
          version = "3.22.4";
          hash = "1x2br9y5n6ipdygk0q6g4maxy49v03rjpwp0dbm8y06b1x8qjd92";
        }
        {
          version = "3.23.8";
          hash = "06cyf3513flahlxckkkfgk2kz5lxcvjfjy7nggllwnn7awyfl726";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "type-fest";
      versions = [
        {
          version = "4.10.2";
          hash = "1j7z063dap6k1fa15b3fksci2j1hli2zsvv935dcn869c4rp6fi7";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "chalk";
      versions = [
        {
          version = "5.3.0";
          hash = "1s9jw9vj09n9wmyljva6h47wv7h2bbfy16c61443vac7g7l1zbh2";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "commander";
      versions = [
        {
          version = "12.0.0";
          hash = "11w7j8hhyws22vk2plyyw6d3axckqq6mbfcfsdfzrsdc7as596p1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "axios";
      versions = [
        {
          version = "1.6.7";
          hash = "1b4rs9877ka24vll4c99w0m63np0mr75970myl1d9fb5imfhcnpq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "date-fns";
      versions = [
        {
          version = "3.3.1";
          hash = "1lnlzbnri4d1czp1l12irmcr6pgj0izphdnza83bp9v1c9yz763g";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rxjs";
      versions = [
        {
          version = "7.8.1";
          hash = "0jfim4x91kgic1q55j5rki3z48pg7k4md4904d8hhzdb4mviccn5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "immer";
      versions = [
        {
          version = "10.0.3";
          hash = "1zd1wzvq7ixjx7b0vb5jby7brg1b8babs1fqbilcvy4s58kp0x8w";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uuid";
      versions = [
        {
          version = "9.0.1";
          hash = "0zizkrc1adacql0mjffdhpd62qk8cysywnz493mgixyrlwaz99lz";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "debug";
      versions = [
        {
          version = "4.3.4";
          hash = "1kwbyb5m63bz8a2bvhy4gsnsma6ks5wa4w5qya6qb9ip5sdjr4h4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "left-pad";
      versions = [
        {
          version = "1.3.0";
          hash = "1pc1siibc9nw5xxqgrmqmpya47k5gs5d0cl89y6sa8v217hhy347";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yup";
      versions = [
        {
          version = "1.4.0";
          hash = "1yiry1iclmbr72iz7hsy34a77w6lz41rdjhv9db4x0234b662d24";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ws";
      versions = [
        {
          version = "8.16.0";
          hash = "05bz1myr773jri4xgzlcdw55xhkhhbjmga7mwnyklmqbja4n18zv";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "@types/node";
      versions = [
        {
          version = "20.11.0";
          hash = "1asp3cb60i9gbv8hx9mvsg72wg7jfm9hnw0yf7q3phikap4v9p0q";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fp-ts";
      versions = [
        {
          version = "2.16.5";
          hash = "1ppmv7c5xdgbr0b70yixsq7pwm3n6bj4qrs7mx6gd7q93jz3hpw6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "class-validator";
      versions = [
        {
          version = "0.14.1";
          hash = "0gz9r29a8ynqsq5bfjkxdflchnsg26j49i7l22h4knjq2kp9ahg1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "reflect-metadata";
      versions = [
        {
          version = "0.2.1";
          hash = "0nd1n3fpkwv273y9gllx8wmfs95fh725sbmw8gwg6nnddg3669ld";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-limit";
      versions = [
        {
          version = "5.0.0";
          hash = "1yp5k64s2jv5cfw3jgs6v63mackmzswy2m03zlcxs3qdn1z9z9xq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dayjs";
      versions = [
        {
          version = "1.11.10";
          hash = "00bdkqksakcd5yzmbiw29r90905pym5by9piidxq00xs2vk79clv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "requests";
      versions = [
        {
          version = "2.31.0";
          hash = "1qfidaynsrci4wymrw3srz8v1zy7xxpcna8sxpm91mwqixsmlb4l";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "click";
      versions = [
        {
          version = "8.0.4";
          hash = "1nqa17zdd16fhiizziznx95ygkcxz4f3h8qfr4lb2pvw52qxfn44";
        }
        {
          version = "8.1.7";
          hash = "1pm6khdv88h764scik67jki98xbyj367h591j8hpwy4y8nnm766a";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pydantic";
      versions = [
        {
          version = "1.10.14";
          hash = "19nw3627hdnli2f7x3r1vlh24s7aa3pazwwn122yfzg25y1ppwa6";
        }
        {
          version = "2.6.1";
          hash = "1a8zbm510czjnfa6xn56w80q121pk7qpywrjdlzcd3a8la1c3mag";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "flask";
      versions = [
        {
          version = "3.0.2";
          hash = "0zfbxxgl5zpbvswxywrr6fam6rj0vknv317flx84484rnzs06b42";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "attrs";
      versions = [
        {
          version = "23.2.0";
          hash = "0c0zjwcqzbmpl93izm2g37gc3lsbbb9pf275fv7zcqn256sw6pck";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "sqlalchemy";
      versions = [
        {
          version = "2.0.27";
          hash = "1y1l4lwhvgs7ivwhcp4vljjdsaha77x9859kz65virhzlxlyv9l6";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pyyaml";
      versions = [
        {
          version = "6.0.1";
          hash = "0hsa7g6ddynifrwdgadqcx80khhblfy94slzpbr7birn2w5ldpxz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "python-dateutil";
      versions = [
        {
          version = "2.8.2";
          hash = "11iy7m4bp2lgfkcl0r6xzf34bvk7ppjmsyn2ygfikbi72v6cl8q1";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "six";
      versions = [
        {
          version = "1.16.0";
          hash = "09n9qih9rpj95q3r4a40li7hk6swma11syvgwdc68qm1fxsc6q8y";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "rich";
      versions = [
        {
          version = "13.7.0";
          hash = "1yh3ajzm9bg6xmp1gncap7h2fhhix4b6h92489c71vprbhxi5daw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "typer";
      versions = [
        {
          version = "0.9.0";
          hash = "1ckq57dbmwwrlralga5zic6bsqi61pqqyh70m18lfbzakbbjz4jh";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "httpx";
      versions = [
        {
          version = "0.27.0";
          hash = "1d9ajsibv9lbg030nrcg4ama5av44x66x5gf0i78gp1jdyj8ijx0";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "black";
      versions = [
        {
          version = "24.2.0";
          hash = "1528xc3fjp6wri3yxx1vgqb7xf08435mp0g4mi6mwhy34xfg5r5w";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "more-itertools";
      versions = [
        {
          version = "10.2.0";
          hash = "1q9rq9g026m4wl6ki2q8pw7xbc02vl34qqw702h9jgixqj0b9k4g";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "tenacity";
      versions = [
        {
          version = "8.2.3";
          hash = "12ncyrgw06bnyyad3a1g9lwi26bfz6zw1d0zgh040gz6g06yz62k";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dataclasses-json";
      versions = [
        {
          version = "0.6.4";
          hash = "0xv3g9jygrg571zx2vplgfn27pgkqk5k094slz660rck4jznwsbk";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "structlog";
      versions = [
        {
          version = "24.1.0";
          hash = "05kvwpl6n8vv3dmaiq23zwivgync9dkwkddrvidz4pfmwj39i821";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "jsonschema";
      versions = [
        {
          version = "4.21.1";
          hash = "1r9f3g6g6zfh0w9b2jl6nkcfsfs0lqm8s8z6vfzacpwz4w07qwl5";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "cattrs";
      versions = [
        {
          version = "23.2.3";
          hash = "17rbcx8rvbdisb3vqac59yqv9q4rhqx7wddc3n8rxambjl6hjd59";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "beautifulsoup4";
      versions = [
        {
          version = "4.12.3";
          hash = "0l8hg3vz9q5fx7gav8sj5zr90d5k7xpc91c1fhhhs1ywis9d3qvl";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.code.gson:gson";
      versions = [
        {
          version = "2.10.1";
          hash = "1hdwnyzzc3yfahrpdk8a77pkhiq8d1z7fif29hcywrs23xfcrqgf";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.guava:guava";
      versions = [
        {
          version = "33.0.0-jre";
          hash = "1r9n03z54v866mpdzgf295vk03sjdl6p99hs15nn12jyg08xj5qc";
        }
        {
          version = "32.1.3-jre";
          hash = "1wq9kbg3li7fb16ixasb4h61n59iddzfvpjds526dwza5lxk6vwz";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.commons:commons-lang3";
      versions = [
        {
          version = "3.14.0";
          hash = "0vwv55y7g1id6rxqhhbf5p2rww8xkhgggaj3prnh5wcqp2pqcfxb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "junit:junit";
      versions = [
        {
          version = "4.13.2";
          hash = "0fkyxafxyfnzbdln478vvx5ykykz7jskq1kb0i6flh1d93v1s61l";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.junit.jupiter:junit-jupiter-api";
      versions = [
        {
          version = "5.10.2";
          hash = "1vxm86j0sma81lw9758jajp86gnxwgkch404gw49vdw45r60a7jm";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.fasterxml.jackson.core:jackson-databind";
      versions = [
        {
          version = "2.16.1";
          hash = "0b4fqip94bbzrsa0x6xgh8825j339dic4q3fxp2vkglc04204fci";
        }
        {
          version = "2.15.4";
          hash = "1530lc2br16g8vn7dzc7i3brdchi3brw3m9j3nq4y3cgmhmkayxi";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.projectlombok:lombok";
      versions = [
        {
          version = "1.18.30";
          hash = "0wdcksgld0k32zbbnb9znb4ckk16bjnd4idm9524iv6096rkl76l";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.reactivex.rxjava3:rxjava";
      versions = [
        {
          version = "3.1.8";
          hash = "0mv7b18v3r8hkd8sqnnqrmwl3fgrwbzp46127rnarifcdp13v2r8";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.slf4j:slf4j-api";
      versions = [
        {
          version = "2.0.12";
          hash = "11q7jm4y0wgs6xd28n1b5q1qagvn1qhqs8m8bgpfv1s8jbjm4l7h";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "ch.qos.logback:logback-classic";
      versions = [
        {
          version = "1.4.14";
          hash = "0s5lvxwda9g4h66ljiyi28wxsay6zgxwcm2z8k3czwm8nv671rmk";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.squareup.retrofit2:retrofit";
      versions = [
        {
          version = "2.9.0";
          hash = "0yjgcjkffr0nim4arw2giqc7qa2ihaj6drrpy9nq9afwiiyv7nk4";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.httpcomponents.client5:httpclient5";
      versions = [
        {
          version = "5.3.1";
          hash = "16hcims98gjri84cv82jwl9b919x2205wybrm2j2dkysx3nkd6zx";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.dagger:dagger";
      versions = [
        {
          version = "2.51";
          hash = "10rymgi6zn1kdrk02p0g08s0drv6gn6bxaprnh0ldndd4zxjppr9";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.mapstruct:mapstruct";
      versions = [
        {
          version = "1.5.5.Final";
          hash = "19m302dirk7mmjrlr0kk5sm4g3sg2a0xlzv4sdd4n90bb87jr8pg";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.vavr:vavr";
      versions = [
        {
          version = "0.10.4";
          hash = "06482zfpivh1k4q8mhz3iww2dilclqwivzflxqdwdzn93rh1z5fm";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.assertj:assertj-core";
      versions = [
        {
          version = "3.25.3";
          hash = "1yrxmizpvvdqkbq11gy1xpcnwyw80x8vd8hcxnz184hrqqi848yc";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.h2database:h2";
      versions = [
        {
          version = "2.2.224";
          hash = "1mpqm9b4gfr89ryp2m982kfljzswsw0x8n456l490f0qijl0vvx9";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.mockito:mockito-core";
      versions = [
        {
          version = "5.10.0";
          hash = "02g3zm0kj9wrb46y0h7aqk8qmkx7mmx5iidzfm2s17ai7xy6qhfy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "jakarta.validation:jakarta.validation-api";
      versions = [
        {
          version = "3.0.2";
          hash = "1296a70gcr6g0rbdyfxzyck67ajs5cgnsif5vwc1gygh71vr3pvp";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.kafka:kafka-clients";
      versions = [
        {
          version = "3.7.0";
          hash = "0mgpmiplx7p70hj8v8y8kw491ckkw322wkndk6frhqy6c1y2aims";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.reactivestreams:reactive-streams";
      role = "dependency";
      versions = [
        {
          version = "1.0.4";
          hash = "025pcjdqbwvylchvi3p6zq814wjd6zmry4dy9r1qqs9njnp3cyjs";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.code.findbugs:jsr305";
      role = "dependency";
      versions = [
        {
          version = "3.0.2";
          hash = "0fq6mai14sg5rj1swxfc90xha0qnqwl4iiqxb5m8qw6hfbi8b7hw";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.checkerframework:checker-qual";
      role = "dependency";
      versions = [
        {
          version = "3.41.0";
          hash = "0kvxgglf6nw4h264fr346wdcrygshpk1va86z94jpqflpl5j4243";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.errorprone:error_prone_annotations";
      role = "dependency";
      versions = [
        {
          version = "2.23.0";
          hash = "0gwpm6rcj38x102pm48abl9pin5p1g7kkn3wp0j3qgdrkdh08iav";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.j2objc:j2objc-annotations";
      role = "dependency";
      versions = [
        {
          version = "2.8";
          hash = "1xf39hzy20p48mbx7jv6sjp7zvfx1rlarx9pi2h5650i3zafw4vl";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.guava:failureaccess";
      role = "dependency";
      versions = [
        {
          version = "1.0.2";
          hash = "1q3qd46ad5zzcwv5g2bhywzk34n18kqcxhxjxybm7g655rgglfyx";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.hamcrest:hamcrest-core";
      role = "dependency";
      versions = [
        {
          version = "1.3";
          hash = "1pw41yfc7d8avknlik2jsvnkqg6n491ck344m1bn1mmgzgcd48z2";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.hamcrest:hamcrest";
      role = "dependency";
      versions = [
        {
          version = "2.2";
          hash = "1bv6sva9k547m7b1d91l7zv563vqym0zgmqdm68iynbhpiynk7pl";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.fasterxml.jackson.core:jackson-core";
      role = "dependency";
      versions = [
        {
          version = "2.16.1";
          hash = "0k50v6yvg0h44vffd6ndx7f1qsqj51d7f7srlvwdf0kxvsq39lqv";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.fasterxml.jackson.core:jackson-annotations";
      role = "dependency";
      versions = [
        {
          version = "2.16.1";
          hash = "1qvb1xwzp6mr652f7338m8vwn9pbr46r97pv5qaklnwd34abggff";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "ch.randelshofer:fastdoubleparser";
      role = "dependency";
      versions = [
        {
          version = "1.0.0";
          hash = "1p6whsjnfqisr84xigywrivkp73ackk1pjdvlw9qcsb715dwr4hc";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "javax.inject:javax.inject";
      role = "dependency";
      versions = [
        {
          version = "1";
          hash = "0gj31rfs2a5kwyi2k18spisjxszicwcpi2j9mwyrq4qwj7i7xf64";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.vavr:vavr-match";
      role = "dependency";
      versions = [
        {
          version = "0.10.4";
          hash = "1zk6kjdvn1xb0axla8qqqhdmj21spbbk95k3h54ifcs3h5rbizpz";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "net.bytebuddy:byte-buddy";
      role = "dependency";
      versions = [
        {
          version = "1.14.11";
          hash = "147r6rm9r8d19ijfxpivzxn0ypzr8m2dd0639haaz6qla4zbv9hp";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "net.bytebuddy:byte-buddy-agent";
      role = "dependency";
      versions = [
        {
          version = "1.14.11";
          hash = "1m6f652inczc0d86drkmwbfgj1brvn9a7bl0fhya908g7gg1hdhy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.httpcomponents.core5:httpcore5";
      role = "dependency";
      versions = [
        {
          version = "5.2.4";
          hash = "1sc2kjpfmcnbb1278v2l8a19046h1iy23z0g8qvl1q28ygk1wijh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.httpcomponents.core5:httpcore5-h2";
      role = "dependency";
      versions = [
        {
          version = "5.2.4";
          hash = "0a50mqqwr1a7h0bj6hhqwm9k46k4s1jbpfyqi9b0yxnaaf76k1ki";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.objenesis:objenesis";
      role = "dependency";
      versions = [
        {
          version = "3.3";
          hash = "078kzhk45diz7bwnlq850lrxs510ha1ddwnfjgqqwb00rbw68qfh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.opentest4j:opentest4j";
      role = "dependency";
      versions = [
        {
          version = "1.3.0";
          hash = "1cpbq7ak977mb3x8yi321kjp32k32nd3h4clmkmxarw2lvij8jkj";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.code.findbugs:findbugs-annotations";
      role = "dependency";
      versions = [
        {
          version = "3.0.1";
          hash = "0b4raj0c1sllnm5lk7bawkcr9kxsy9s4w76bdz6kpvl5cribrkrk";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "net.java.dev.jna:jna";
      role = "dependency";
      versions = [
        {
          version = "5.12.1";
          hash = "12hqxpq1p6dqbb9p67agjdhxjs6vg4mcqldi5y4gb3s738h37y98";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "net.java.dev.jna:jna-platform";
      role = "dependency";
      versions = [
        {
          version = "5.12.1";
          hash = "0vlzn8yva7dc1k1vb0pgpzy91ci83z92acsk64fg59ij0fanqivy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.brotli:dec";
      role = "dependency";
      versions = [
        {
          version = "0.1.2";
          hash = "1hzvhnxnwjbasmsxrga8p52b0fvf9gs0arcv8z81yp273zjc2jh6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.locationtech.jts:jts-core";
      role = "dependency";
      versions = [
        {
          version = "1.17.0";
          hash = "02nlg0ri8hw7f5d1r7xd689narqr0yaz2rly6l7ngmz90a2al1nh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.osgi:org.osgi.core";
      role = "dependency";
      versions = [
        {
          version = "5.0.0";
          hash = "05bh3plafkl7h5qhr4r2xjxx3mdf1lwzyvk5n8smw8l83ih7kmfc";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.osgi:org.osgi.service.jdbc";
      role = "dependency";
      versions = [
        {
          version = "1.1.0";
          hash = "0sb9zw06s478jzlm6w0962z8f5k3sr1ibr15c5f0g9py673lpa9c";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "jakarta.servlet:jakarta.servlet-api";
      role = "dependency";
      versions = [
        {
          version = "5.0.0";
          hash = "1mb5jkhh04zs21nkj6lqg93jwwqdy3icl6246jxlh2mpwiw6s1rg";
        }
        {
          version = "6.0.0";
          hash = "17ggmspwgifpcybl6vxsy4li1rm0gg3f0zibibvlnahmfvwjdhix";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "javax.servlet:javax.servlet-api";
      role = "dependency";
      versions = [
        {
          version = "4.0.1";
          hash = "1mj8hz55df9pvqacy7bsc8i4wja6r2n9w11s1bxmllw8mrhng0m2";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.lucene:lucene-core";
      role = "dependency";
      versions = [
        {
          version = "8.5.2";
          hash = "134dr2rrpxzw3dqmnjiya4dxlyl3vlxdb1b29n6x7b3sxf5mgj4w";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.lucene:lucene-analyzers-common";
      role = "dependency";
      versions = [
        {
          version = "8.5.2";
          hash = "14565h3dck3idqmz86c226fcarzk2sxy6iwi1hfcf5kkv97my2fd";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apache.lucene:lucene-queryparser";
      role = "dependency";
      versions = [
        {
          version = "8.5.2";
          hash = "1wmmmf8lkknd6b4kvphfjybq5zndnz0b18s8n5siqwxrs8c6glcw";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.github.luben:zstd-jni";
      role = "dependency";
      versions = [
        {
          version = "1.5.5-6";
          hash = "0nl5j2qq9k16fn9m1y151rng2ijdblddh8cl66q9pd739iils94b";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.lz4:lz4-java";
      role = "dependency";
      versions = [
        {
          version = "1.8.0";
          hash = "03v58chzwbsbvlrk72yh6xx3g9fl5mbvr74g0yvx198bv2i0kb2k";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.xerial.snappy:snappy-java";
      role = "dependency";
      versions = [
        {
          version = "1.1.10.5";
          hash = "1jnns992yw8w3nzlvrzv5yyzvg5xc5cx3rvcrbc90k7g3vlv8q15";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.bitbucket.b_c:jose4j";
      role = "dependency";
      versions = [
        {
          version = "0.9.4";
          hash = "0qg2kni0y79a4rq0ylr37bgr5xyfbkpibnyay4fif8rx8xpw881b";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.opentelemetry.proto:opentelemetry-proto";
      role = "dependency";
      versions = [
        {
          version = "1.0.0-alpha";
          hash = "06cv7hvrmdkw0kv9bddlcdxdwn2gq461kxxw9gnb9sjxcyr09wp0";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "com.google.protobuf:protobuf-java";
      role = "dependency";
      versions = [
        {
          version = "3.23.4";
          hash = "01bdbl22j5frmidw7pv3435dmhjvwhxvrhp7nmsj4n65i4w02n7j";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.conscrypt:conscrypt-openjdk";
      role = "dependency";
      versions = [
        {
          version = "2.5.2";
          hash = "03vj3xwgds09q6h7ha2h9000ajsyfmixp52g9j2z1xz7v20kg8r4";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.apiguardian:apiguardian-api";
      role = "dependency";
      versions = [
        {
          version = "1.1.2";
          hash = "0bayd1xs45syys5hpw5zbvlj2ri7qx6k4nv5nsz1fa212m1plyi7";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "org.junit.platform:junit-platform-commons";
      role = "dependency";
      versions = [
        {
          version = "1.10.2";
          hash = "1miasymgjyydrmv8l15gfq9xnw2ikb9r64rcdjhijdl4ykw5qhz1";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "jakarta.mail:jakarta.mail-api";
      role = "dependency";
      versions = [
        {
          version = "2.1.0";
          hash = "1ix4qwvh9g3nnnnyriy5r4qbhjnh1xvzzsskcjab8nvzs776p42j";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "jakarta.activation:jakarta.activation-api";
      role = "dependency";
      versions = [
        {
          version = "2.1.0";
          hash = "0kjj4wghmani8ki6a7w0qb4114y3kxpdqsz9f9ryb172w6bqj0s3";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Newtonsoft.Json";
      versions = [
        {
          version = "13.0.3";
          hash = "0xrwysmrn4midrjal8g2hr1bbg38iyisl0svamb11arqws4w2bw7";
        }
        {
          version = "12.0.3";
          hash = "17dzl305d835mzign8r15vkmav2hq8l6g7942dfjpnzr17wwl89x";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "System.Text.Json";
      versions = [
        {
          version = "8.0.2";
          hash = "1pi1dkypmn34qqspvwfcp1fx78v0nh78dpdyj4rcaa2qch40y15r";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Serilog";
      versions = [
        {
          version = "3.1.1";
          hash = "0ck51ndmaqflsri7yyw5792z42wsp91038rx2i6vg7z4r35vfvig";
        }
        {
          version = "2.12.0";
          hash = "0lqxpc96qcjkv9pr1rln7mi4y7n7jdi4vb36c2fv3845w1vswgr4";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "AutoMapper";
      versions = [
        {
          version = "13.0.1";
          hash = "0s23aqxpiv86kx83hlfdkjwwg22n3fss24ix0kvm2g9fm9anrffy";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "FluentValidation";
      versions = [
        {
          version = "11.9.0";
          hash = "1ayjznpgl891625h60hjjkcpl287ppshzihp33hi4gqgznaqwzwy";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Polly";
      versions = [
        {
          version = "8.3.0";
          hash = "1pmh6iwkzgbxn62k1g1agwzgqdbq8g0yj5wslyxknpri6pyx9y5c";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Dapper";
      versions = [
        {
          version = "2.1.35";
          hash = "1lxkbiip51bspzkh7xmafdcn5dzka08jmqfn5rrfv53v5k4yisnd";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MediatR";
      versions = [
        {
          version = "12.2.0";
          hash = "17s76w5c2gd2mvsdpn02dqifa62q7pn98ff0m3v559w7sna2apv7";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Moq";
      versions = [
        {
          version = "4.20.70";
          hash = "0jzfxvw5ngxld2rfzq1361lqzi3f8shywqd4546ayz7wgga1vq9v";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "xunit";
      versions = [
        {
          version = "2.7.0";
          hash = "0qs7yaz8qdhi75is7grgdxwxm09j36wv9c2ifyj2xd5jfzvlkc71";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "NUnit";
      versions = [
        {
          version = "4.1.0";
          hash = "0fj6xwgqaxq3mrai86bklclfmjkzf038mrslwfqf4ignaz9f7g5j";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "StackExchange.Redis";
      versions = [
        {
          version = "2.7.33";
          hash = "0hdqw5z95b8f5l8zkgpiv45w6snv9hykv48ywr9ab6gzkr3gi64j";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Microsoft.Extensions.DependencyInjection.Abstractions";
      versions = [
        {
          version = "8.0.1";
          hash = "1wyhpamm1nqjfi3r463dhxljdlr6rm2ax4fvbgq2s0j3jhpdhd4p";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Humanizer.Core";
      versions = [
        {
          version = "2.14.1";
          hash = "1ai7hgr0qwd7xlqfd92immddyi41j3ag91h3594yzfsgsy6yhyqi";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "CsvHelper";
      versions = [
        {
          version = "30.0.1";
          hash = "0v01s672zcrd3fjwzh14dihbal3apzyg3dc80k05a90ljk8yh9wl";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "RestSharp";
      versions = [
        {
          version = "111.2.0";
          hash = "1lsphw5f006qnmf5895g9idifdlbyfqq9l4m27mxhm1c05sz6ykn";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "NLog";
      versions = [
        {
          version = "5.2.8";
          hash = "1z3h20m5rjnizm1jbf5j0vpdc1f373rzzkg6478p1lxv5j385c12";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Refit";
      versions = [
        {
          version = "7.0.0";
          hash = "16v2yvycjyb2828gyrxfgnn7pmkbd8ycrqpjxpghvqvh9as0gipp";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Microsoft.Bcl.AsyncInterfaces";
      versions = [
        {
          version = "8.0.0";
          hash = "0z4jq5prnxyb4p3163yxx35znpd2msjd8hw8ysmv4ah90f5sd9gm";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "protobuf-net";
      versions = [
        {
          version = "3.2.30";
          hash = "08bjdn8dbqpzn5c9fw89y5766irwplgyzhyxcrjzpywkwpj75r4i";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "nlohmann-json";
      versions = [
        {
          version = "v3.11.3";
          hash = "17aa7wqpfdiir7nr85xvw66fa0rln4fxzlq5y9w5rb0r678n2952";
          url = "https://github.com/nlohmann/json/releases/download/v3.11.3/include.zip";
        }
        {
          version = "v3.10.5";
          hash = "13krlljnrjcpxy23azsnbisd916lnw1kfyhd5yvm6rw5d3grfjdr";
          url = "https://github.com/nlohmann/json/releases/download/v3.10.5/include.zip";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "simdjson";
      versions = [
        {
          version = "v4.6.6";
          hash = "0xlcj0mlh2c8xpfp6xh3awydcrg6r6wn2x5hwjr8i39wnh0fc5r2";
          url = "https://github.com/simdjson/simdjson/releases/download/v4.6.6/simdjson.h";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "fmt";
      versions = [
        {
          version = "12.2.0";
          hash = "13w7wndksh55wm47my1n5v5s0sypxmvpy01rqgj59ybq27asix52";
          url = "https://github.com/fmtlib/fmt/releases/download/12.2.0/fmt-12.2.0.zip";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "zlib";
      versions = [
        {
          version = "v1.3.2";
          hash = "05lx27q69pm8vzi5w4ifsy86kq32q1kwcqcxa42ls9yh5h59lcmv";
          url = "https://github.com/madler/zlib/releases/download/v1.3.2/zlib-1.3.2.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "zstd";
      versions = [
        {
          version = "v1.5.7";
          hash = "18vgkvh7w6zw4jn2aj1mp0yv98m4fk52ay6da0wh4pm194gyaczb";
          url = "https://github.com/facebook/zstd/releases/download/v1.5.7/zstd-1.5.7.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "lz4";
      versions = [
        {
          version = "v1.10.0";
          hash = "12zlqlp7j3fri1bvwfpz7637cvf6iv7mq18j54imxcs48y814xak";
          url = "https://github.com/lz4/lz4/releases/download/v1.10.0/lz4-1.10.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "cli11";
      versions = [
        {
          version = "v2.7.2";
          hash = "1cl2ssaakn277jhs7775gwp1piq9dlgfhpjhfalrw4n7qi2fzc0c";
          url = "https://github.com/CLIUtils/CLI11/releases/download/v2.7.2/CLI11-2.7.2-Source.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "googletest";
      versions = [
        {
          version = "v1.17.0";
          hash = "0z5j23gpwmmfp07f8yc9pzyqn46j672csjn1fz5ki7c2v40vgyk5";
          url = "https://github.com/google/googletest/releases/download/v1.17.0/googletest-1.17.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "magic-enum";
      versions = [
        {
          version = "v0.9.8";
          hash = "08fmq2i4dycnw2snx5v7fp0zpkyh7vbp1503bsknhwb9m749sw48";
          url = "https://github.com/Neargye/magic_enum/releases/download/v0.9.8/magic_enum-v0.9.8.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "abseil-cpp";
      versions = [
        {
          version = "20260526.0";
          hash = "037a3bm5ps86v51ady018wdcgpj00b2fpr43pxj42hbkai9yw6kf";
          url = "https://github.com/abseil/abseil-cpp/releases/download/20260526.0/abseil-cpp-20260526.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "glm";
      versions = [
        {
          version = "1.0.3";
          hash = "1cnygx77acwgim4bhpf2hd66vzqc96z40kn9gdxprn5hv770y2hw";
          url = "https://github.com/g-truc/glm/releases/download/1.0.3/glm-1.0.3.zip";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "catch2";
      versions = [
        {
          version = "v3.15.3";
          hash = "1jhv6gm922gksjkdy262bnjc1icjr61j89rcq7vj50l9x7szqgdj";
          url = "https://github.com/catchorg/Catch2/releases/download/v3.15.3/catch_amalgamated.hpp";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "spdlog";
      versions = [
        {
          version = "v1.17.0";
          hash = "0i57sdy8agmi5xpdh454jwfv2a14bmhb307mnd35hknpqrajk1nq";
          url = "https://github.com/gabime/spdlog/archive/refs/tags/v1.17.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "cxxopts";
      versions = [
        {
          version = "v3.3.1";
          hash = "0wi0kv1pdvi7qzc8n14dn9wsa5l92w4dhab4lialn7aj5ia71z1v";
          url = "https://github.com/jarro2783/cxxopts/archive/refs/tags/v3.3.1.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "tomlplusplus";
      versions = [
        {
          version = "v3.4.0";
          hash = "0maisabrwnz1fad5va7gnpgwmh9q39ikdfwfryfaxym471czc5w5";
          url = "https://github.com/marzer/tomlplusplus/archive/refs/tags/v3.4.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "date";
      versions = [
        {
          version = "v3.0.5";
          hash = "01dvnh3n3xz8biz22lpw667d0yr4myrl0ml2fmjcgbix43f6wy7g";
          url = "https://github.com/HowardHinnant/date/archive/refs/tags/v3.0.5.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "cereal";
      versions = [
        {
          version = "v1.3.2";
          hash = "0pzqh7mfgdankqa18dbask4fphs3ybbbaqjxqpd80n5s66dsv9qn";
          url = "https://github.com/USCiLab/cereal/archive/refs/tags/v1.3.2.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "range-v3";
      versions = [
        {
          version = "0.12.0";
          hash = "1jx99dpqi84zp38c9khm96pl5x9p6gnbw987mz7dz3m900ixnnh1";
          url = "https://github.com/ericniebler/range-v3/archive/refs/tags/0.12.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "cpp-httplib";
      versions = [
        {
          version = "v0.52.0";
          hash = "1fvjxxranbicpmqxlhxjbyw96zlhnxpg2jg29la8lc59diqns3by";
          url = "https://github.com/yhirose/cpp-httplib/archive/refs/tags/v0.52.0.tar.gz";
        }
      ];
    }
    {
      ecosystem = "cpp";
      name = "argparse";
      versions = [
        {
          version = "v3.2";
          hash = "167cd02snfvirhl4wdyxa8n6l6wz3asm9albmi42l6x4w263vjwx";
          url = "https://github.com/p-ranav/argparse/archive/refs/tags/v3.2.tar.gz";
        }
      ];
    }
  ];
  jpms_modules = [
    {
      name = "org.slf4j:slf4j-api";
      versions = [
        {
          version = "2.0.12";
          hash = "0izdyyncrag2h2dlyr9mdlnni220d8i92xm28ql75gfzmfw055d7";
        }
      ];
    }
    {
      name = "ch.qos.logback:logback-core";
      versions = [
        {
          version = "1.4.14";
          hash = "14x22xk3v4f68ij3k3zda1w1d0005xww21wmfd91h2sk89gz1hpq";
        }
      ];
    }
    {
      name = "org.opentest4j:opentest4j";
      versions = [
        {
          version = "1.3.0";
          hash = "06rgfkgjss4qfprhpvrnn9y64m93pf5gzkadsv766rdbdiixzqj8";
        }
      ];
    }
    {
      name = "org.apiguardian:apiguardian-api";
      versions = [
        {
          version = "1.1.2";
          hash = "0f0bnysya4gil4r1hx7ch9sh0waxngq3f98qkwqhgmh6qn5482dm";
        }
      ];
    }
    {
      name = "org.junit.platform:junit-platform-commons";
      versions = [
        {
          version = "1.10.2";
          hash = "1bnq3z3xhlgl7ryr1j0lma7xpq4gr4jbm2xifd4xyyd40305wsmm";
        }
      ];
    }
    {
      name = "org.junit.jupiter:junit-jupiter-api";
      versions = [
        {
          version = "5.10.2";
          hash = "1k0p9fdh1y3cib3r3ga0mkb1z05a6d8zlwiqh1sp4cfdhv0pgzxg";
        }
      ];
    }
  ];
}

