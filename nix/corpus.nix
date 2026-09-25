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
        {
          version = "0.16.1";
          hash = "004i3njw38ji3bzdp9z178ba9x3k0c1pgy8x69pj7yfppv4iq7c4";
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
        {
          version = "1.0.229";
          hash = "1fp04fq4a79bpm61xz1zy0pbz4kpc7d771zii1k3inmszq55jj21";
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
      name = "automod";
      role = "dependency";
      versions = [
        {
          version = "1.0.11";
          hash = "057sa45859nb8arbshkqc6va8b8jf5a8vx6zr739viibqbj989md";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ryu";
      role = "dependency";
      versions = [
        {
          version = "1.0.23";
          hash = "0zs70sg00l2fb9jwrf6cbkdyscjs53anrvai2hf7npyyfi5blx4p";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "equivalent";
      role = "dependency";
      versions = [
        {
          version = "1.0.2";
          hash = "03swzqznragy8n0x31lqc78g2af054jwivp7lkrbrc0khz74lyl7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "indoc";
      role = "dependency";
      versions = [
        {
          version = "2.0.7";
          hash = "01np60qdq6lvgh8ww2caajn9j4dibx9n58rvzf7cya1jz69mrkvr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ref-cast";
      role = "dependency";
      versions = [
        {
          version = "1.0.26";
          hash = "0vdra0766jcc2czzqwhql41kkfyajdnai1pbkjxbq8vr7mvqyvi1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ref-cast-impl";
      role = "dependency";
      versions = [
        {
          version = "1.0.26";
          hash = "0g70ff9an5i97cw9kijgzqrqydz7smcfic2zyydddizfbxl874ic";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rustversion";
      role = "dependency";
      versions = [
        {
          version = "1.0.23";
          hash = "07z2a843fs80fawwflj9jwn49k9b0bd0dhhbvy0ar69vaxd72m6g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_bytes";
      role = "dependency";
      versions = [
        {
          version = "0.11.19";
          hash = "1a1y1v0r9akqyvprxnmpgc0i8wybqqpvgi01mi8qxn3rkrq41m55";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_derive";
      role = "dependency";
      versions = [
        {
          version = "1.0.229";
          hash = "0j4k63i7h1bikxwz2c89ig0hrwbnl9mz1czn85xx99x5cc9dg9g7";
        }
        {
          version = "1.0.196";
          hash = "0rybziqrfaxkaxrybkhrps7zv3ibxnjdk0fwais16zayr5h57j1k";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_core";
      role = "dependency";
      versions = [
        {
          version = "1.0.229";
          hash = "0j1ajiha76h3nmd976il9li6975k121xa7jb39ws8n0yqp4s5p37";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_stacker";
      role = "dependency";
      versions = [
        {
          version = "0.1.14";
          hash = "0jhgpgcki4gqa8z28g1bxliz6ilf9wsak4r2ybpyfjqcsmsn74yl";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "stacker";
      role = "dependency";
      versions = [
        {
          version = "0.1.25";
          hash = "0rwrws4iyh8cay7pghzf2dy55pprwfn1vm805f5czfh6cza4jzvh";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cfg-if";
      role = "dependency";
      versions = [
        {
          version = "1.0.4";
          hash = "008q28ajc546z5p2hcwdnckmg0hia7rnx52fni04bwqkzyrghc4k";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "psm";
      role = "dependency";
      versions = [
        {
          version = "0.1.32";
          hash = "08m67yndaikwq8kviipphi50lfb25ph7j3gp4w3rffz6k52h7kad";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ar_archive_writer";
      role = "dependency";
      versions = [
        {
          version = "0.5.3";
          hash = "1dhjwdapx0ydx38r7aj8w4rqmdhxs1xl2zp8xala0h11zzg5ikbk";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "object";
      role = "dependency";
      versions = [
        {
          version = "0.39.1";
          hash = "16vkcaamik55jd9f04g73hvgsm5w636gb4w06x3nafvsih4nqnif";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cc";
      role = "dependency";
      versions = [
        {
          version = "1.4.3";
          hash = "0v9b5arr047vbihfbh3fmbd3aj9vf1i7dbdgfpvlwzynpjvr35ah";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "find-msvc-tools";
      role = "dependency";
      versions = [
        {
          version = "0.1.11";
          hash = "145qpfb9r4ml2klr8v4byvrkikp61qyiks9n69b8z0vbscbb0pfl";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "shlex";
      role = "dependency";
      versions = [
        {
          version = "2.0.1";
          hash = "1fjsll1cd7d2bcpdij9kd6w62rpbc7qqzvydvs021vsmr1cxvypq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "windows-sys";
      role = "dependency";
      versions = [
        {
          version = "0.61.2";
          hash = "1z7k3y9b6b5h52kid57lvmvm05362zv1v8w0gc7xyv5xphlp44xf";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "windows-link";
      role = "dependency";
      versions = [
        {
          version = "0.2.1";
          hash = "1rag186yfr3xx7piv5rg8b6im2dwcf8zldiflvb22xbzwli5507h";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "winapi-util";
      role = "dependency";
      versions = [
        {
          version = "0.1.11";
          hash = "08hdl7mkll7pz8whg869h58c1r9y7in0w0pk8fm24qc77k0b39y2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "trybuild";
      role = "dependency";
      versions = [
        {
          version = "1.0.120";
          hash = "1r38znl4w0l9d0fh709fndmxza4dpvw89sd4icyncmwkngv5nq0y";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "dissimilar";
      role = "dependency";
      versions = [
        {
          version = "1.0.11";
          hash = "0gh99yfpjqv5gqzk2fwfm8c7ncl1r7lwkfgjhcmgviar82midnmf";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "glob";
      role = "dependency";
      versions = [
        {
          version = "0.3.4";
          hash = "02zby4rsidb2ksrnysyrsaap7rk6wpp7vl5chflndafhl5gaisz4";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "target-triple";
      role = "dependency";
      versions = [
        {
          version = "1.0.1";
          hash = "0wfixgm6scp13s2di1kz3sl3cc1gg0gscl27s9rgmbcr7p7bz9n3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "termcolor";
      role = "dependency";
      versions = [
        {
          version = "1.4.1";
          hash = "0mappjh3fj3p2nmrg4y7qv94rchwi9mzmgmfflr8p2awdj7lyy86";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "toml";
      role = "dependency";
      versions = [
        {
          version = "1.1.4+spec-1.1.0";
          hash = "1xanf3v10j8hdjz37mkhg80w92cw25kxwndhcp4w5pxw9czydb1s";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serde_spanned";
      role = "dependency";
      versions = [
        {
          version = "1.1.1";
          hash = "09jzk7i6wihn3d8i3wi4j4n98ghi93c3b8m8k64nxq0ijn3vaqk6";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "toml_datetime";
      role = "dependency";
      versions = [
        {
          version = "1.1.1+spec-1.1.0";
          hash = "1mws2mkkf46l7inn77azhm0vdwxngv9vsbhbl0ah33p2c9gzcr9i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "toml_parser";
      role = "dependency";
      versions = [
        {
          version = "1.1.3+spec-1.1.0";
          hash = "0mjdvihdkmjd4ykh574xgii71hpxw7ns7h4n4bisqpxrz4faqf0x";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "toml_writer";
      role = "dependency";
      versions = [
        {
          version = "1.1.2+spec-1.1.0";
          hash = "1lk6pqf9mac3v1x6282n6a66qx5b18c8f4a23bsd0nk658x3amkx";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "winnow";
      role = "dependency";
      versions = [
        {
          version = "1.0.4";
          hash = "10fzxipa7lx16172p3aca9j60hzbqgjki2f95kqksd5qywcp7f93";
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
        {
          version = "2.0.119";
          hash = "15vjy620l91a3q4n4f4gzhnflmdr6pnm38v2m6cpk86i8av32a47";
        }
        {
          version = "3.0.3";
          hash = "18srnql3cd39j9q6hf1az02p67rlr1rf6njx9zx4vxj9i3jvmsak";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "proc-macro2";
      role = "dependency";
      versions = [
        {
          version = "1.0.107";
          hash = "1nb6ly8kp65f724kj73ippc7lvydss24sm2vagk6qpklpg4pwplq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "quote";
      role = "dependency";
      versions = [
        {
          version = "1.0.47";
          hash = "00ch0yyzvv6s671ik0kcsbw8nigdaj2g3fr61kcahwx48aqlvgqz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "unicode-ident";
      role = "dependency";
      versions = [
        {
          version = "1.0.20";
          hash = "01lafj17xwizrlvn006zz8ip99hqisf77kjk0a8flfmpmrsynbj6";
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
      ecosystem = "go";
      name = "google.golang.org/grpc";
      versions = [
        {
          version = "v1.83.0";
          hash = "06q1h39siffkcpfkhbfp4p9zajjvpqd01f1r1yw5dp3h3yn5kybl";
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
          # The published sources classifier omits lombok.spi.Provides and
          # five test-harness declarations. These are exact source files from
          # the matching upstream tag; flake.nix overlays them into this
          # checkout. They are source input, never the compiled Lombok jar.
          source_overlay = {
            url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/src/spiProcessor/lombok/spi/Provides.java";
            hash = "1pgwld0l7zg1q8c06iv7s97i5pa4yk4lkyx4raaajanwp5wzfkdv";
            target = "lombok/spi/Provides.java";
          };
          # The Maven sources classifier also retains two published test
          # harness entry points in TestBase.java/TestJavac.java, but omits
          # their suite declarations. These are source inputs from the
          # matching upstream tag, not classes copied from the Lombok jar.
          source_overlays = [
            {
              url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/test/bytecode/src/lombok/bytecode/RunBytecodeTests.java";
              hash = "1dcmjpq8cj6bdcxk2sxz23kkw9991ba3j0gqby7aiz6j69hhs2rf";
              target = "lombok/bytecode/RunBytecodeTests.java";
            }
            {
              url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/test/configuration/src/lombok/core/configuration/RunConfigurationTests.java";
              hash = "0kp37qbc3yhz2hlg7j0cf3wh1hw6w7vin9ivmcsswa7l8cnbnamg";
              target = "lombok/core/configuration/RunConfigurationTests.java";
            }
            {
              url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/test/bytecode/src/lombok/bytecode/TestClassFileMetaData.java";
              hash = "0mifxawgas91lsb1a2azf0v4hrk6zmajr17hfycfwl24k91jllnh";
              target = "lombok/bytecode/TestClassFileMetaData.java";
            }
            {
              url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/test/bytecode/src/lombok/bytecode/TestPostCompiler.java";
              hash = "1fnnwjr1bi285c2siazqyyqfqxzhpd1fzmz42ikgclajfhmlwwd2";
              target = "lombok/bytecode/TestPostCompiler.java";
            }
            {
              url = "https://raw.githubusercontent.com/projectlombok/lombok/v1.18.30/test/configuration/src/lombok/core/configuration/TestConfiguration.java";
              hash = "05j6qszpc1dm23yps77nr8y1zlxcmd13qiqz1kd4inwz5kn6mdja";
              target = "lombok/core/configuration/TestConfiguration.java";
            }
          ];
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
    # The NuGet artifact above is binary-only. Keep a separate, real upstream
    # source checkout for Roslyn occurrence/reference coverage; it is never
    # used as a compiled dependency or a fabricated test target.
    {
      ecosystem = "nuget";
      name = "Polly.Source";
      versions = [
        {
          version = "8.3.0";
          url = "https://github.com/App-vNext/Polly/archive/refs/tags/8.3.0.tar.gz";
          hash = "06qk0npv3dm4gyzlsz2v16s0ygbp5xr7ka1dy7jmwvxwwasmh47g";
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
    {
      ecosystem = "crates.io";
      name = "aether-crypto";
      versions = [
        {
          version = "1.3.0";
          hash = "18aak1vq9vpni44xqj99yzzlfc4hhg7wrh59fc4cfx5df95671ij";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "aether-p2p";
      versions = [
        {
          version = "1.3.0";
          hash = "13ml0k6h3p38rxrlliyiqv4rnv32pcqzx2h8qc4z98f8ww2hbg20";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "agave-shred-wire-format";
      versions = [
        {
          version = "0.0.1-reserved";
          hash = "16zhdp74j47v3wb1a5rpfm8s6b4hgmr2h82l26c8ybiclgi401z7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "agentquay-macros";
      versions = [
        {
          version = "0.3.4";
          hash = "1nzyamjb1mfz9cyc5knac4ak7pz7qzpmr00nlv230j9ry4pc4bb5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "anva";
      versions = [
        {
          version = "0.1.0";
          hash = "0pz9ly1nnaqliw6xma552nljkmk4g6w7i3wgillpr3d4qp5fvgr3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "arna";
      versions = [
        {
          version = "0.1.0";
          hash = "0qg5jrha5k1a7wjxng555n5ac5yff2y65m3b4sp47kf1h9jwrimi";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "assess";
      versions = [
        {
          version = "0.0.1";
          hash = "0g5l24wc1km95y738gf1a6gm8nqhzd9n0wrlylgj0ms3kh2y5rg4";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "astli";
      versions = [
        {
          version = "0.1.0";
          hash = "1kcjs7l6pm89rb9wz5w81j72g651q7d8wh4aqxgkpgppbpmxc1hb";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "astli-diag";
      versions = [
        {
          version = "0.1.0";
          hash = "13hpcvmkyb3mha1c46c30f5nwylzn7cbfxivwrairs9fl409615m";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "astli-text";
      versions = [
        {
          version = "0.1.0";
          hash = "1d7a9lch7064bn9j81cnrflzgxp41jby09d3wxzfl89ac6k2rymg";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "atomic_parser";
      versions = [
        {
          version = "0.1.1";
          hash = "0pk3nnnhhhaa5gr1l0y7phqv9fma76pxjqdzxm9ky2qcbafpbf1h";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "atria-render";
      versions = [
        {
          version = "0.1.0";
          hash = "0bdspig7bba5g2i4dq8wmgaxxpv7nkiw6s4gzf7ha4vpa2hj60xz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_cache";
      versions = [
        {
          version = "1.1.5";
          hash = "1i1agqz11ic7kf2kb9hjn3a2539kwc2hyw9aaln2zzgwzay68qhv";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_cli";
      versions = [
        {
          version = "1.1.5";
          hash = "1phcwkgd6mmkx2zcqq62zg9506bsclz9sll5b9a0xsg3jfv4b8ha";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_config";
      versions = [
        {
          version = "1.1.5";
          hash = "17jwqn7bwlgili3hq5yd2i4yh5yj6v4mcv1kk4w0zqyzdq8pfl45";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_config_macro";
      versions = [
        {
          version = "1.1.5";
          hash = "1072vzjy3kb1zqm1hn8g8iw0s9lkyvp9h2xqz0l0rw8k3l1ai3jc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_kv";
      versions = [
        {
          version = "1.1.5";
          hash = "15n28aqd7ymdf11vp13ixhz6f28xwcxdlhwck73bjjgallkpmzlc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_logs";
      versions = [
        {
          version = "1.1.5";
          hash = "06mvqpzh7r7pxgn57vz87bcjg9bph1vanbdgwfqwa4cc49h64m6i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_orm_macro";
      versions = [
        {
          version = "1.1.5";
          hash = "0km6zcmv4kvfqjmxi94zandrw65izrh7sp66507m12fqvnm0844p";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_session";
      versions = [
        {
          version = "1.1.5";
          hash = "0wc6lb4sn7skvi6dkfsgw29a535y0d2n21wz09x9c3k3gg9rnyca";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bee_template";
      versions = [
        {
          version = "1.1.5";
          hash = "1105h4imh4xyj1730qfhchhp3djs34q4hrkjydkkab2vhzc5mh5c";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-collections";
      versions = [
        {
          version = "1.20.203";
          hash = "0fdcwim0yjdpcszxaxidsmhqarc10xz7zp1s411rm0jscknn6k23";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-derive-refineable";
      versions = [
        {
          version = "1.20.203";
          hash = "0z00bxn1wkb4xlwrvbbpy27gc1cdb7d61dib5ca78mn8vsn1jg6a";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-gpui-util";
      versions = [
        {
          version = "1.20.203";
          hash = "1s2qzjhw00hiqjw738b7hd4h5gbxv0m4qfmhq0sqnk89cly603jv";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-media";
      versions = [
        {
          version = "1.20.203";
          hash = "19zyybfnqz2dcwb08m5wizzkpr0f7h2rgbcjjj5x7ypsrfqba49a";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-refineable";
      versions = [
        {
          version = "1.20.203";
          hash = "1ka62brb3kky75msgnzrfldd8v2g9vmj7ig67nc30acap2691cjr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-shared-string";
      versions = [
        {
          version = "1.20.203";
          hash = "0ij9v0xx0acljqhhvl5040ml4x0039ag3w463df3bxqxhv479nsa";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-util-macros";
      versions = [
        {
          version = "1.20.203";
          hash = "03yjnpz56szhrf1qmv08jcjv1rsi8r8rwxd3imw0blln2li2lh9c";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bite-gp-ztracing-macro";
      versions = [
        {
          version = "1.20.203";
          hash = "1dvccjr8a4kb1bksgydysv8piwbw693izkblrdgnr92izv54xlla";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bkndb-storage-redb";
      versions = [
        {
          version = "0.1.0";
          hash = "10l179gmrg4adzjh3b5d5sh7sn6qvmfcr8r58lw7rxq4p90fz49w";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "blyte";
      versions = [
        {
          version = "0.0.0";
          hash = "0r41hav3ra5mwqq05mviwjbln8hycj3igvbq19b5vghd1q3xskfb";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "blyte-agent";
      versions = [
        {
          version = "0.0.0";
          hash = "03wxzw9xvbzrg3wrg15jgmam591b27c1rhv0mracvb6k6ivswwqy";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "blyte-cli";
      versions = [
        {
          version = "0.0.0";
          hash = "03p99smlqrygn62gz48d223cxn9i30v8zq705w1j3lshgblh419g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "blyte-core";
      versions = [
        {
          version = "0.0.0";
          hash = "1h5vglpkyf8vxq9fhncw62xilxy57ghxpl1zrwmhz9yfisd3z7ki";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "blyte-provider";
      versions = [
        {
          version = "0.0.0";
          hash = "0saisnnlwd1gwnzp88nbv6wa4qxpqc7g1h54vmhj3afhr8rcjg5z";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "bucketlist";
      versions = [
        {
          version = "1.0.0";
          hash = "0lcfx4qx8wdlgh3hkfkicv4z43msyw8izxml2j04cji96nlbn6y2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "byo-alien";
      versions = [
        {
          version = "0.0.1";
          hash = "12mx698pdmvmw9pryxw9s6hdwn4vdykgcwjrlxmqqdmhsqidgzc4";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cargo-issafe";
      versions = [
        {
          version = "0.1.0";
          hash = "0c5srx4w8r6niyqyvly5rhs9wvjvk4xqdp3kr0f0ic72bc0p20fk";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-c";
      versions = [
        {
          version = "1.7.0";
          hash = "0yagsvjmj1i4px65hlxdl94bxjy2f97yjzcha50r01i79d4pz56z";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-clike";
      versions = [
        {
          version = "1.7.0";
          hash = "1i9qg7azfvvsr6p4w9li281pjjz4c4fhny4pgrnf3avijy1kl0fl";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-clojure";
      versions = [
        {
          version = "1.7.0";
          hash = "1magqvnff6hj3l2d9y7p8rp4f3cvw71z1046142y224ain3xj7l7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-cpp";
      versions = [
        {
          version = "1.7.0";
          hash = "0p7n7fvmpdm339jbac10hpm6acf1120507na4frg440karzd3vxr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-dart";
      versions = [
        {
          version = "1.7.0";
          hash = "1kkkqrwdz2w57zn0hyjr77954f3ql2k4k80hbb1v7vp9467wvg95";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-java";
      versions = [
        {
          version = "1.7.0";
          hash = "0alb3n51m0bgmm4lff5q4qh2f31naikg5q48xsfjpapk6k65bwsc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-lisp";
      versions = [
        {
          version = "1.7.0";
          hash = "1a0nqdkh8q0b3inhfc2fpz905j55nx661yhbgid544d16z4xags1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-lisp-kit";
      versions = [
        {
          version = "1.7.0";
          hash = "1qvdm6mdqfzbcqsdkmakr7yq9ia7w4j8ka319na4vannyck2w6kp";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-pl";
      versions = [
        {
          version = "1.7.0";
          hash = "0j6k22wy28gszphy9bw0ji27wbajf958x6jx0agzlrngnrdcqs8j";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-py";
      versions = [
        {
          version = "1.7.0";
          hash = "1df9cg8bfq4x7frkhk9llzpw8blfwjm5qs9dwf2cx0h68qg8i9ml";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-rb";
      versions = [
        {
          version = "1.7.0";
          hash = "12p5xp5fpl8wj2h0yk10pr838d4a17ml9lcyw9rkw59k734hdaf8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-scala";
      versions = [
        {
          version = "1.7.0";
          hash = "1w2k1aakr4qympzzxq76kp4n14qsrnjq7ayazyp5pjhby79r7v90";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-scheme";
      versions = [
        {
          version = "1.7.0";
          hash = "17gbm6nh42chgrx4jk7mzjyph50d5xllazkz4hvbwivr69nmaxm5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-swift";
      versions = [
        {
          version = "1.7.0";
          hash = "03ffslq523m8rq9xgs3w86kvbvld4xzadvg3rsr6nyjmmgpz4kz1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cccc-zig";
      versions = [
        {
          version = "1.7.0";
          hash = "01rpbdfbmm62ibcafb6pn6k0fniddvxvii0nk5y5b4jkn9spkpza";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "codrop-cli";
      versions = [
        {
          version = "1.0.0-rc1";
          hash = "1zgsgcmbl7j6vc9jah60z8b3ir4qwz2arifjf6ns2ja601vgdnwj";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "coldctl";
      versions = [
        {
          version = "0.1.0";
          hash = "0v4y3a5whw5wcj5z7jdyih5c6170p40m5znqzcrx1qmvfm7yqbf1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-asset-build";
      versions = [
        {
          version = "0.1.0";
          hash = "0aqxnyvg9qkkf7fdiknm51hy6qijpavjllzsgc4zbkrnqxxivyg6";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-asset-format";
      versions = [
        {
          version = "0.1.0";
          hash = "06rfl878rqdj96bvixs0kcm32vxsdfj24308jj2m6i7j33piv4ym";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-color";
      versions = [
        {
          version = "0.1.0";
          hash = "0z7s41ackh4qr1pq58ngvb838h0dw1a5dzn0wajsp15f6zwbrri5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-ecs-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "1yshc4cb96l2az4b8g3l3as7vr5kfks3x4h6c0fw804ilfhm6d3g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-editable";
      versions = [
        {
          version = "0.1.0";
          hash = "1hkl1wv9gjd79j1cz7ci5rpcyvfjckx88d051m2dnqfk7f199nnr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-editable-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "018apgch3xc66ms3d6qjc4mfxl61zljvmjvnb3xkx114cqjd8qwi";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-foundation-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "1xqzi8m59gqirh009dvabayaslwcf78g3svrpqdivqsapq9vfgw7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-render-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "1dagdsprvvf1gkmfdx7g1sj08nvi8xf1j0jj29mh9dv9kv9l3p75";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concerto-tasks";
      versions = [
        {
          version = "0.1.0";
          hash = "1g7pnzgyfhf0s89ys11m5qnxpxigbfr834z68ijzizjam71cak1i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "concinnity-derive";
      versions = [
        {
          version = "0.19.53";
          hash = "0c5bzaivmlikvmq8g8zy7kas6a75srjizx7g0l36mdf76axpvw3c";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "consortium-integration-testkit";
      versions = [
        {
          version = "0.2.0";
          hash = "09bc29glgsngz3zbxbfbqbm1g86x415kwvnb8wr9x5vc1kvq635k";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "cranpose-navigation";
      versions = [
        {
          version = "0.1.164";
          hash = "1vg8ql2xinb60bdyqjqw3x48f372b462cl7d9b7m62biz3vald32";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "curve-abstract";
      versions = [
        {
          version = "0.1.0";
          hash = "0khwx0flldcr463xidw7s41wx9wv0mcg2z9yfcfaz4mjk1a61gay";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "decision-core";
      versions = [
        {
          version = "0.1.0";
          hash = "09g4ibh4z4wzspjzdqhv5bkmni5djh7z9h9mmq30h444w5mp9iry";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "decision-jev";
      versions = [
        {
          version = "0.1.0";
          hash = "05mdq1qbhxlhji75cs6k44agngknypfjh7rala89fn712nlw512w";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "decision-openai";
      versions = [
        {
          version = "0.1.0";
          hash = "1m2myd769cmqdz6j68l0if03b6v518a5bjlh446nh3sp9n7l2qh3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "deep_causality_tempfile";
      versions = [
        {
          version = "0.1.0";
          hash = "1w3i80r1axlar3rxgv15jixniipgi7n9d8qapfc0m3xravh3x5pm";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "dist-typestate";
      versions = [
        {
          version = "0.1.0";
          hash = "01j2rdpn3apiwhi75zsj8r8vlikl24namvds6z3ixqhic684j338";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "distributed-typestate-macro";
      versions = [
        {
          version = "0.1.0";
          hash = "18hkkixhvss5kkdrqvsdbn292zl3n9vf1d0fs59vlgr6z8g342ic";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-bench";
      versions = [
        {
          version = "3.0.3";
          hash = "0y5ln8rxrdx8g8mrfi7kfd3hf3yv1pqr352f8575xghh39h0xdah";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-circuit-breaker";
      versions = [
        {
          version = "3.0.3";
          hash = "0yphmky4jadlq3bc2rbyh1p41rak7sj7mkq4n7c1dlb9b89ny8l8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-cli";
      versions = [
        {
          version = "3.0.3";
          hash = "0vwrpzj54ws2lvz5hgba06vqy7qmc79q9rcpad96qkfnmgn3lzc5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-client";
      versions = [
        {
          version = "3.0.3";
          hash = "002fs4hb2l79v59m1dn9vhqqrcv279ygigq8zpa7zqbz6qvmsx2x";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-config";
      versions = [
        {
          version = "3.0.3";
          hash = "092yc7042q02rpd10s3xnqm5cj2q34dghd7589sgdhydp5n3amc9";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-config-remote";
      versions = [
        {
          version = "3.0.3";
          hash = "0x61chbd1vyswix3mg97qa5s8ravbi224ygbm7q5j3225g94qina";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-data";
      versions = [
        {
          version = "3.0.3";
          hash = "1wm6jhpidwkmjq180iibfyb9kh6i1x1pyssckir2j2g6767nrww7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-data-memcached";
      versions = [
        {
          version = "3.0.3";
          hash = "1jgj7aav7r42q1pzacxb2hj3zm19f5n1baanfsqs5qssfjfxh0y6";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-encoding";
      versions = [
        {
          version = "3.0.3";
          hash = "1lmp4npc4454gkl587z5lhfddgk3yr0bz77bym1r0gixajqcigcc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-errors";
      versions = [
        {
          version = "3.0.3";
          hash = "0m1nvn6wqmshsd39pag8gcgf1y29avgj9gvp8j2baqsc1511xshd";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-events";
      versions = [
        {
          version = "3.0.3";
          hash = "1c69p5j1xsl85npmi0z2i92r41djvkdb6r5si0zcm1ys94kp91r0";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-health";
      versions = [
        {
          version = "3.0.3";
          hash = "054c95rli6qjysycxlfsyhn04higz3nwzjr1nx3slf0a3ip83ghc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-lock";
      versions = [
        {
          version = "3.0.3";
          hash = "0050nal4972isv6vk4piw7qjxgbvlhm0vfywvyix11rmcknijwzd";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-logging";
      versions = [
        {
          version = "3.0.3";
          hash = "0czgli9wfrlgqbx0rq9i2319qfnw7dfzda4mbnw9qdpljrqy7f8z";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-metadata";
      versions = [
        {
          version = "3.0.3";
          hash = "0m61i8wmr14q0nvmr6h7hwzy1ix3b28f9lh46b023bsm174ly6xv";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-metrics";
      versions = [
        {
          version = "3.0.3";
          hash = "10cbh8f15hcwgzglqcx2fhia4rqrid05fw149yxr2r6lfjaa2jad";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-mq";
      versions = [
        {
          version = "3.0.3";
          hash = "1wvs4wwk3j45x0cwwcbivcn2c5ij6h7kp54jak6w1fihsazw47d0";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-mq-kafka";
      versions = [
        {
          version = "3.0.3";
          hash = "0kmcwdljqflx7fn5fxy9m0hlfbn1sml9s96b6s6l5a1x0p9sd6nq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-mq-mqtt";
      versions = [
        {
          version = "3.0.3";
          hash = "185wcibyszrqddy4lxi4dmdsb2qyv2pf503x3j371iaps3vg6b6m";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-mq-nats";
      versions = [
        {
          version = "3.0.3";
          hash = "1zcbrkiszlj29yb6s5fvvnw9hr7afjv8d6wdgfxn06wr04n31r60";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-openapi";
      versions = [
        {
          version = "3.0.3";
          hash = "092kki7jclqgfhwd7f0il9qrpg1qfblflnp6pfmm2598cgp1gw7i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-protos";
      versions = [
        {
          version = "3.0.3";
          hash = "0fa8g30pc86f2j5fmasfx377a5m702wm74j046vz4ifysdxlifwz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-registry";
      versions = [
        {
          version = "3.0.3";
          hash = "04fb25ds7h99lbhs2w9zpcjjfnxhxvb7fmjsivarga45cl8lz10w";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-registry-consul";
      versions = [
        {
          version = "3.0.3";
          hash = "0k9175ca41bh1yak5p54knh4h1zmw0hk38dfx53vkwb1i9xpkh9g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-registry-etcd";
      versions = [
        {
          version = "3.0.3";
          hash = "10q40sdivn40hnnz3yc2jdgb90gfyvwr6mcp6jxhr7638g3q7q8r";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-scheduler";
      versions = [
        {
          version = "3.0.3";
          hash = "1hbfrccz7rbnzpibpi931dc3brhw22iiy5hacac2vz0fj10zyrqq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-security";
      versions = [
        {
          version = "3.0.3";
          hash = "1hvl6q945iz247qhwb93xlhnm8jl6lg971v78kz94vrl2s4zl699";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-tls";
      versions = [
        {
          version = "3.0.3";
          hash = "1w4ccymwzyn9yncff4jla1hddkxangb098alms9vbxirgjyjgbpg";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-tracing";
      versions = [
        {
          version = "3.0.3";
          hash = "1bxdh2444fggy75h2cvg3cjvp66f3lr46k8xx02953mzrx3rksk1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-transport";
      versions = [
        {
          version = "3.0.3";
          hash = "01l1z07xs0z1rdlwsi1dh82c26ix4f25rbixxlprl22kimb1pids";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-transport-ws";
      versions = [
        {
          version = "3.0.3";
          hash = "0155kf6qw9iyiarvq7989sgq6pnpzg6mykj6mg8q55j1jv8c2sq8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ecat-versioning";
      versions = [
        {
          version = "3.0.3";
          hash = "12lw7hw2vwins3xnyfvbns3p8ypjipcyk8y9wgv0jfdabvpwn637";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "echovalidum";
      versions = [
        {
          version = "0.1.0";
          hash = "1bhn7d1cvwp9nqib0r7qz508zapk6cd0lkknaac8ckcgnn5a6xkz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ekos-plugin-sql-dialect-clickhouse";
      versions = [
        {
          version = "1.0.3";
          hash = "1ijs0r8pgf2fjsbvbwfy1jgycckd3fdd44zxby6x82qn0gv0yv1r";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ekos-plugin-sql-dialect-databricks";
      versions = [
        {
          version = "1.0.3";
          hash = "1lp6d7cmlnhrcvhnjdhdwx80zy2xl97rnahg60mqzcqf662fj6qp";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ekos-plugin-sql-dialect-mssql";
      versions = [
        {
          version = "1.0.3";
          hash = "0n53x2ja95yzrr7l6jh329wbdv5wafkv767avjkbrwwi9hw89jkr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ekos-sql-dialect-sdk";
      versions = [
        {
          version = "1.0.3";
          hash = "1kk64fjs02sh01s0pyffq2pq0ic1dbp26qswwq83x3jl765k0vfw";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty";
      versions = [
        {
          version = "0.1.0";
          hash = "16pp607d4qhz29nw0bk2cwfpkvlrwxlg2bbk3s40g632mihyjvl8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-apple";
      versions = [
        {
          version = "0.1.0";
          hash = "1kxd20svlw8r570hf6k7qh5chxjlgr07wrp7d3z6ibr59j14rfr1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-back-handler";
      versions = [
        {
          version = "0.1.0";
          hash = "02jhk4ifl7y15gihzg6r7n6v0c7cnc1bmv2slf0ap6qinjcrks03";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-instance-keeper";
      versions = [
        {
          version = "0.1.0";
          hash = "1p6v7x7fdylp2fjrshk1fb0d90g1bhavf5j57dfnrpksi7f45sm5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-lifecycle";
      versions = [
        {
          version = "0.1.0";
          hash = "08g4qjyaxchfmr1asa8kkb728ap5pd49zi7799iadh743qrv85gy";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-state-keeper";
      versions = [
        {
          version = "0.1.0";
          hash = "1y99gsydxka2wkzpkg5k6v1fiwj9qyvrpwlhzfjhskys73vjshzk";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "essenty-web";
      versions = [
        {
          version = "0.1.0";
          hash = "1c03gz58akisw1sic1spmig8y4q0z1z8m24sshnqx1cqad9q31k4";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "fastdb-sys";
      versions = [
        {
          version = "0.2.1";
          hash = "1r7sd502m5bydqxgr1b0li55mq0l2s8nqvm5zxhdz2mpkhv9zdj1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "fastmash";
      versions = [
        {
          version = "0.0.0";
          hash = "1mzv4k4fhr83k72mzfh95392zzppr7pn22fmav9vy44fy30lj685";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "fluidattacks-gitlab-sdk-rs-domain";
      versions = [
        {
          version = "0.1.0";
          hash = "18ykjjcyrmildxzsq84aysm1y6ayz2df81q95pq8jdzbka11wcd9";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "frus-image";
      versions = [
        {
          version = "0.2.1";
          hash = "0lf0rmpywxfy5azzhprr1xbdyx3ix6r8aggg8i8dif9qmjx037r5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "frus-l10n";
      versions = [
        {
          version = "0.2.1";
          hash = "07q7cpy1w8sjbk070cml21yzz9ffssm2qghvqy9j8rw2q1zp00qf";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "gdtf-share";
      versions = [
        {
          version = "0.2.0";
          hash = "0zsw1gbxa11g4l36a21f4caadbv20kww4xc9097vff7w4bklnxsl";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "gohtml2text";
      versions = [
        {
          version = "0.1.0";
          hash = "1b29khgsqhj4n4ljms0qy4332n7b7npvqavli951fffpyhfwssjn";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "harness-threads";
      versions = [
        {
          version = "0.1.0";
          hash = "1d03y4qisdrdm5i6k3dcgdsksslwfj2qfqh51nz8gyzqcpiwn239";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "hirust-router-macro";
      versions = [
        {
          version = "0.1.0";
          hash = "0i71g330q40vykb5gmcysz8r9jnjk67hzfz4jkbnk23gfl3afbcm";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "hirust-wsock-macro";
      versions = [
        {
          version = "0.1.0";
          hash = "0lkivjwcgzd78pknnys2mwbna9n97q45b4989gaacyrhxbhw2lxb";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "honeymaker";
      versions = [
        {
          version = "0.0.1";
          hash = "11nn83yn3g2p1j9xpavlj79ysjmb8kdrd13dzwax4q9m638gdnc9";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "hotpath-drain-meta";
      versions = [
        {
          version = "0.0.2";
          hash = "1l87dwwldawjh821ayb024sfrbmjbmyanq24jb8cp0qz8gmlf60g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "identus-core";
      versions = [
        {
          version = "0.1.0-rc.1";
          hash = "085qyckdbw1x8avprkfpiaki14jn4p8yrz40fxbjmd7zfg9gjazr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "inillucent-sqlite-reader";
      versions = [
        {
          version = "1.0.29";
          hash = "173ym7l1jigqi51scf8wprxhs0mhyyy120apv7579cr0g2r05ypz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "jolt-diplomat-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "14jlg9pz08zcksyly3jxvkyqm98yibs4zzqx1aziq6wk10navcy7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kcode-k1-daemon-lib-testkit";
      versions = [
        {
          version = "0.1.1";
          hash = "05cwg1zrpbamkwqjxhl6bixjqcf7qacqnd10pfky5kqxzmiyxabm";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kcode-k1-daemon-startup-error";
      versions = [
        {
          version = "0.1.0";
          hash = "1b457bacjgg8n2mxr9r7vgwiih02f4ql90k148vs8z06wrfbpli2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kcode-k1-http-access-profile-presentation";
      versions = [
        {
          version = "0.1.0";
          hash = "0hqys8hs6ybkw8r59b953nsh4y7g3nh358iapria1c9v81mw8k7f";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kcode-k1-http-access-profile-presentation-representation";
      versions = [
        {
          version = "0.1.0";
          hash = "1llsv7sn7fl214g8cynvydc69h3f49qb9ig0ag1wmdvb8lqp04dq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kcode-k1-web-selection";
      versions = [
        {
          version = "0.1.0";
          hash = "0wgsbr9vpn81x6v7aqx7l84vzv3ajfaln4s0s87w0wr6gzs8mmla";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "khoj";
      versions = [
        {
          version = "0.1.0";
          hash = "0jd2mkpbb0q481yhp11mvcq6h7zmrj94p4gbrm69911a2qrfgrp2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kineti-actions";
      versions = [
        {
          version = "0.3.3";
          hash = "0x1wngbh9n5s7r27pmvczl2386ins1bmcrx6fq1bhjhygx85758c";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kineti-reflex";
      versions = [
        {
          version = "0.3.3";
          hash = "048cyqdpf4l79n16k2xqjscllnddj7a2c5f9304d0320fq6rl0h3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kittycad-point";
      versions = [
        {
          version = "0.1.0";
          hash = "10cmjnbs92w4vpjdndi9m7s627bcq5cmfij1fbk2ga55xh29l807";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-actor";
      versions = [
        {
          version = "0.2.0";
          hash = "0mh6h8dfpsdm6bdb5zm4wwxsidczazma9mmaarrc7m009axs6sav";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-api";
      versions = [
        {
          version = "0.2.0";
          hash = "1mnf6dd9sh82dwjhqcwkx5cqf08vqnl9hky7a1j7cj7768w91gij";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-core";
      versions = [
        {
          version = "0.2.0";
          hash = "1mngbf2ng77v503drabp17vj39a7vfhjydqz9ql1cnqklg8ly2cx";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-gui";
      versions = [
        {
          version = "0.2.0";
          hash = "082ykri3fqm7h1rdaxxayqgrsim5fv38224ff0lrnx0k4r2amd8p";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-router";
      versions = [
        {
          version = "0.2.0";
          hash = "0811gaa67idsqf2giza3xlp404h2wvs62w2m3ljj30y9fg39zkz3";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "kova-sandbox";
      versions = [
        {
          version = "0.2.0";
          hash = "0c8q6cv37s4bl8ym7zwb43jd3nfbpcacj7bfszv1bv51n135mard";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "lns-artifact-contract";
      versions = [
        {
          version = "0.1.0";
          hash = "1gvcbpyy7hqjwidimw09vpw9rz0h1l3z051y3nk6rckhn92517sq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "loongio";
      versions = [
        {
          version = "0.0.1";
          hash = "11z0cs9dxqifbkpdglj950g0piv260r2vw1zljvqpf1z78jjjbn2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "lt-lindera";
      versions = [
        {
          version = "0.2.0";
          hash = "1vffsq7ixm98wsl9xwrma9ykp6v97ikk735p1ca3042hcbrz1wsd";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "martensite-pdf";
      versions = [
        {
          version = "0.19.0";
          hash = "02nyn6q0khs991fyf3ikiw7655phwnjaaq2nrblvswzywysv4x9w";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "martensite-webview";
      versions = [
        {
          version = "0.19.0";
          hash = "1l09y96z1f6zilqv673zfzxj1sin1vn5ak1raq92xzfgnisb747v";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "martensite-webview-platform";
      versions = [
        {
          version = "0.19.0";
          hash = "0hanfgjdy4zwal7jgcvvi8wfykdl5msgigi73lb5jcq6kjhrka3s";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mdrv-ds-shell";
      versions = [
        {
          version = "0.0.1";
          hash = "1s3ndijxjs7njqv1aswfw7w9dpcdxndc6izd8jv5w806iqcz4pl1";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mdrv-gpui-collections";
      versions = [
        {
          version = "0.0.260925";
          hash = "0c9sc7xilg5hlw2ycbz6sxqpmn886bad97qmpliwby1hr89rygbq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mdrv-gpui-derive-refineable";
      versions = [
        {
          version = "0.0.260925";
          hash = "03jifp588vxsmw6cyzzdli69h6iwwm4r09nnma2y46h867mfsh6i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mdrv-gpui-media";
      versions = [
        {
          version = "0.0.260925";
          hash = "1275xqq5af7x74xb1c9ns1ksdcckvb7ginpsg6xkxbj09alj4rbi";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mdrv-gpui-shared-string";
      versions = [
        {
          version = "0.0.260925";
          hash = "0c7wbvmm7jnqaq6v4m6bi3imwnpcx9s80m28shazwhbmqadki349";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mhome-face-matching";
      versions = [
        {
          version = "0.1.0";
          hash = "17sfs2r6yqr5nf2mih6mcabxykw0ryyhg5vcj94l0bs6a1iq9j1x";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "mhome-person-api";
      versions = [
        {
          version = "0.1.0";
          hash = "18jvyggmbh27lm7dp0nq96rqsjy79xsf5s4ds97r6ipsfbsl5qky";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "midici-responder";
      versions = [
        {
          version = "0.1.0";
          hash = "1cx11vp9b888xn3gfvl9hwkndzfdzdckjgz93gd3kggg8gsyhyyi";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "midici-transport-alsa";
      versions = [
        {
          version = "0.1.0";
          hash = "11dxxfjp195gzj8qgczax9d2g87j1mph4pp3imq6cgd7wlwvpdwh";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "midici-transport-clap";
      versions = [
        {
          version = "0.1.0";
          hash = "1jpwai9lkrhqa9g96vbcqxga3xdyyzhxssi3jmp6xlg6hyd3380b";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-abi";
      versions = [
        {
          version = "0.2.0";
          hash = "0gmp89cpp3casn3i63b490f8f7wjirk6xf1h2yyp5nrkf7jwx8nm";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-gpu";
      versions = [
        {
          version = "0.1.0";
          hash = "0hknzj2g62m9fhrm1q0pk11v2dr396hscqgh8kfdlfszs9zgsklw";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-ir";
      versions = [
        {
          version = "0.2.0";
          hash = "0y16yqrx23xfv1xky0ayrfm02pgg6rqz8f2dv3kb0vp2219lcqwr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-kernel";
      versions = [
        {
          version = "0.1.0";
          hash = "1pw68l93iiqksi8pydw8isfavkdv2cfj8gqsqy0hcqhzfvx5pvj7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-macro";
      versions = [
        {
          version = "0.2.0";
          hash = "0i4zy2ada1r9pvd15zd3pry2cx43r73c2z3x01a29hl1sk8laqw5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-op";
      versions = [
        {
          version = "0.2.0";
          hash = "1as4n3xwghas7jrrd3f8igsz35y7srqyav939wpmmb90syp09x4g";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-precision";
      versions = [
        {
          version = "0.2.0";
          hash = "0vdxbys1cr8j61f79i744qsgm4c8a6sxrkpqv0ri9nvjdbv8fhwk";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "neura-shader";
      versions = [
        {
          version = "0.1.0";
          hash = "0kj2nhyjjmb7x775pg6dlvl7l9frikzp1fn0pr1qryi9wvggyy0q";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "nibbles";
      versions = [
        {
          version = "0.1.0";
          hash = "00q8s74hcdzrh69s6l63iqrclhc07cflrscgd5ikq5c986yhk230";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "objc2-accessory-access";
      versions = [
        {
          version = "0.0.0";
          hash = "087g6ikqy2750gviychhby709xxx2ca6p7w853ahmg54qj3rhhqq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "objc2-av-system-routing";
      versions = [
        {
          version = "0.0.0";
          hash = "1l68h0ans9inj3zgmgb72h1p50nd12pwka49cnvncv6ckmvcs29l";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "objc2-link-security";
      versions = [
        {
          version = "0.0.0";
          hash = "1jzx8mw02dc07ifhb6l7ay58i0znqd9x9rw6drdif4v2hgkv54i7";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "objc2-state-reporting";
      versions = [
        {
          version = "0.0.0";
          hash = "0l04b30k8nrsikj61yf7qpfvg94c7isyc4s8y6h3v93xji3x40if";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "obsidianlog-core";
      versions = [
        {
          version = "0.2.0";
          hash = "0snyk03bljh99ncwd2slayqaz3nzh7dy6p8rgc10v26ha9h4pas2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "oii-derive";
      versions = [
        {
          version = "1.0.0";
          hash = "0fak18hawbp1s220hxn80hrnyl23w35nrm1gs13k8n7qh942hg3r";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "paganel-plugin-sdk-macros";
      versions = [
        {
          version = "0.1.0";
          hash = "1iw4sgx293rswq6qb5msfjdaz4b0s2xm94p0j31p172mj2qjj412";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ponyo";
      versions = [
        {
          version = "0.0.1";
          hash = "13bff88pi3jc2zqkhnwv3x13fcbx2jgxfwrd258dig7m82vrifc8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "projjson";
      versions = [
        {
          version = "0.1.1";
          hash = "1g51l6gwdnr2jzn2pf22zm0fw25j5qaml7qi1ki5pzwlz6nmr493";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ps2_bios_checksum";
      versions = [
        {
          version = "1.0.0";
          hash = "0k9avihviskldifz96alzirvlj6nb0w1jw597wn3gf91pzs0ivll";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "pux";
      versions = [
        {
          version = "0.0.2";
          hash = "1b63y2lxqh81nyimgaaizxqb4pyzg2kys7rn1c6bhzpkqym6j86y";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "quilt-canary";
      versions = [
        {
          version = "0.1.0";
          hash = "0qzd5mrkgfjyhi0baj4aww7vrbvalk0my8wj4lnxkq1sc0z6bdbr";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "reallyme-compression-brotli";
      versions = [
        {
          version = "0.1.0";
          hash = "16s78smvxy59diss2lfmvd1svamqwi4xixzn8xxqwx47rqblhy3w";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "reallyme-did-types";
      versions = [
        {
          version = "0.1.0";
          hash = "1d8axq0pbxs1av33sd7838bm9sb0kiqnaknnbyi6wn462xbv02ds";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "reallyme-vp-core";
      versions = [
        {
          version = "0.1.0";
          hash = "150vv903s6vafc7ipakm87qr3kxnl2y7l6fxw3n1xxh0ypv97i74";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "redis-tower-auth-aws";
      versions = [
        {
          version = "0.1.1";
          hash = "0j6zzyy6ab17k05f8a1bynqr8mggydlfisqvqp7n2v2amw3z878d";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "reglyco-pdf-wasm";
      versions = [
        {
          version = "0.1.0";
          hash = "0j3rz55pm6inivwfni1bc3l0d8rvqfpxbiir21b0gnjj8l19d50f";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ribergshamra-core";
      versions = [
        {
          version = "0.10.0";
          hash = "1kf1qpikd52hm14wb30r15s9n15xr6mdd5shf073k5va0b1bmfmz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ribergshamra-xml";
      versions = [
        {
          version = "0.10.0";
          hash = "0i86d92icf8yaqg5rcnf9bz583l53wbgdmznai1z83xxnk0d3kxw";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rip_templating";
      versions = [
        {
          version = "0.1.2";
          hash = "1pybybx4bwjbpva3hnlc91zl0z337xyj0a14jsnk2w3yygpyzsh9";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rip_templating_macro";
      versions = [
        {
          version = "0.1.2";
          hash = "1f16jms98dlqb5cxih4zy2qg5nfxm200q220k071lmb4va7q92gd";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ripscope";
      versions = [
        {
          version = "0.0.0";
          hash = "1mc228zgffkyhls7nirl3yc5idnbglcimdprpv33f2q87dv09h93";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rl-pm";
      versions = [
        {
          version = "2.2.0";
          hash = "01bxhz0ipx8kqipkmz2f3alljwywi9qwbwx4m3fa2hb702nxygq2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "rs-rich-macros";
      versions = [
        {
          version = "0.0.1";
          hash = "1qlwcvh9xdd5a6h33dhmzrjdrxz69y1ff5k8gbfj04vf3sndkpqg";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ruff_command_line";
      versions = [
        {
          version = "0.0.15";
          hash = "1yh5wi3nzlhd5wh65kglv412hg5nhwnjm6yzs92ylhg4acp0yd44";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "sbol-db-search-sdk";
      versions = [
        {
          version = "0.1.7";
          hash = "1mwghkl4vgwwq18mag17sdxw9fwf4zjrha15i7xiv09a1ila785n";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "sbol-db-vector-conformance";
      versions = [
        {
          version = "0.1.7";
          hash = "1hgx8k9cgj3bsgix30l7i7qj8b3gig0713p51gpcv5my5by2qz7r";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "sbol-db-vector-flat";
      versions = [
        {
          version = "0.1.7";
          hash = "1a6ry26bm1havpdk4ghjvrr69vdz63s33pxdrdl606zwa1m6i6gp";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serc";
      versions = [
        {
          version = "0.0.0";
          hash = "1rd7hb5kncgz6082zj4z4ilk26s0x4j4sifa40z89qaqpg235xym";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "serc-cli";
      versions = [
        {
          version = "0.0.0";
          hash = "0nm6mpww9fk3wc0fr4fpmkpw90hd5fbac8a19xkz6w8qaarhmjad";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "spirv-std-nightly";
      versions = [
        {
          version = "0.0.1";
          hash = "0ydpmh6yqdav5pyai2ff4if7x3kbbms590bvpiqd8mgf620rhd7l";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "subc-jobobject";
      versions = [
        {
          version = "0.1.0";
          hash = "14hl40vak7syn7lr1kyy2saf5sbb6a0ivayz2baghgb1i7gwnwip";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "sva-fingerprint";
      versions = [
        {
          version = "0.4.0";
          hash = "10vd3rhsb3vi6nxx5jm5g4a2q43dvdwxdgpwk12ibzjlbgkvd3dn";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "svarog-curve25519";
      versions = [
        {
          version = "0.1.0";
          hash = "0w6pb43p9hdgrf6gpy6dcgpvy4jdxz206hglr1dyx1qcyisk5yw4";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "svarog-lagrange";
      versions = [
        {
          version = "0.1.0";
          hash = "0s3gi4rrlmfcrhbj8cai8r722hqw7x1320bibill4pyg63asskxs";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "svarog-secp256k1";
      versions = [
        {
          version = "0.1.0";
          hash = "0kzlnjpf5h3p00l29z4mjpm15lbidap9y13n31fga7s0h9qifskj";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syntrop-contextd-core";
      versions = [
        {
          version = "0.3.0";
          hash = "0l74a3r9wqb3wi4vgz2svbfqhkk8mzp9nvd6hlixqmmmybh7y3vn";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syntrop-runtimed-core";
      versions = [
        {
          version = "0.3.0";
          hash = "1dnlkc83m3ba8k33fj5lz6sh0dabk6x4fizfqgk7dmwk7vf3i1yv";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syntrop-toold-core";
      versions = [
        {
          version = "0.3.0";
          hash = "13c9fz5qq28l01pz4kzwv3kaddwddyhz3yz3ni2vva4nkcv7l7ll";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syntropctl";
      versions = [
        {
          version = "0.3.0";
          hash = "12jrcsdhp6rb2rnjflpdhfzjb7myd0jd99dzqpqv1px3n7ikk3km";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "syntropctl-core";
      versions = [
        {
          version = "0.3.0";
          hash = "1wydv28mkbg28m39s3y1rxkxy6xyg138k7jg6fmrpmzx299mv259";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tc_stream_cipher";
      versions = [
        {
          version = "0.1.0";
          hash = "0fvj8lqb74zwfy9zpadlkwz0mqk6x7ixi1bihbypzvhd4i50lils";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tfrs";
      versions = [
        {
          version = "0.1.0";
          hash = "0m8q9ysxvmqvv3qhdil7a9cah8aw8hbk3wn1ir89400lwhzs24kl";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "thinkthen";
      versions = [
        {
          version = "0.0.1";
          hash = "0amjn4ddb0fmcaamgyfp4g1j5zc59pswcj9wp3bh9rsv17wn3hjq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tinosrse-tools";
      versions = [
        {
          version = "0.2.1";
          hash = "1mgwha7gywlglj61karwigvqwrz3qijc4lfgacbfjpzpcgzq8k1f";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-canonical-form";
      versions = [
        {
          version = "0.1.0";
          hash = "16v14jcn1gz7mksadsj1bpkrz3s6gjmks2fvqn8l38g8sadhn6c8";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-core";
      versions = [
        {
          version = "0.1.0";
          hash = "14609607bjfzvpy946jcwgaszm3g7s6b0sz4d17vipz03cdmi4pq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-ir-kernels";
      versions = [
        {
          version = "0.1.0";
          hash = "1f5ab0dipi38h8v5mndqyd8nfwxhl5fgw137n8k1q2s9wvii9gna";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-libs";
      versions = [
        {
          version = "0.1.0";
          hash = "1jpzxp97b6kymxk0vwcx7vga9y2wljbbqg1v74ixqj2il49dkcvp";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-optimize";
      versions = [
        {
          version = "0.1.0";
          hash = "1r38l9km5sx0lpl9rdn045fq15iifbm5z2jhi8b5faqd12yk7ss5";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-rebase";
      versions = [
        {
          version = "0.1.0";
          hash = "1hhqr1yhf1ynvjfr5k5k0812rx0qz4h501d55sdpxb1ymcpzcxnh";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "tk-pg-utils";
      versions = [
        {
          version = "0.1.0";
          hash = "1l4nqc2932pjwzh6m3mypmpa5g6pz53712cwlc596axhyck92367";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ttdal";
      versions = [
        {
          version = "0.0.0";
          hash = "1r8m065b5k1hwkxz1m6v6cgbcz9h28xlfsrf4s4asd350l65ij5h";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ttdal-sys";
      versions = [
        {
          version = "0.0.0";
          hash = "03v9sz67xhk5svqf2jgk0jkx81a5xvl23wllrnrzcvk6hn544xbq";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "turbojev-benchmark";
      versions = [
        {
          version = "0.28.3";
          hash = "1y6dqjn56nw3vq26dns188k8r8b4g3d8i09n22r8aqn9cd3l2n0v";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "turbojev-specialized";
      versions = [
        {
          version = "0.28.3";
          hash = "0s6y1frd5di5vs4xwrq94m9x6xmrs1r03g8rfi0dmg4gpzhpq372";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "turbojev-specialized-artifact";
      versions = [
        {
          version = "0.28.3";
          hash = "1qgqq77dr64v15yncapfhj1zbshy8sc649vf25hzgnlilzs6bnzm";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "txwhy-verify";
      versions = [
        {
          version = "0.1.0";
          hash = "0gpi4b73xfgrnqd3sd9iwazv1w782ahw0v6dy9vqbspzxs3rgz0n";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "udpstp";
      versions = [
        {
          version = "0.0.0";
          hash = "11fm73g8mwv5cpixx230g67fdzwcpc2mc94s367lgq3ypyr75kif";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ulo";
      versions = [
        {
          version = "0.0.1";
          hash = "1kcwk9qys4l8bf810plc30bxjbcvzidsxnwh975r8mhmaq3lac7i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "uo";
      versions = [
        {
          version = "0.0.1";
          hash = "0qhgfn2vldy2wh6c76ss5rz09k2dfdyhrnh1kml24myrr6k0hja9";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "utf_types";
      versions = [
        {
          version = "0.0.0-reserved";
          hash = "1ja1cy9480xzdl7kaflr8amza6zqi7z3q93mr8ydwmyg2q7l2k7y";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "web-checks";
      versions = [
        {
          version = "0.1.0";
          hash = "0v3a0d4c70blawnhinljap8jkvndvs5hy4xix2m6jr7zq4syflbc";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "websec-ast";
      versions = [
        {
          version = "0.1.0";
          hash = "1pn2cpm5vhfqvmi98iiq78ph0pgx1lznqsvhpxzd6xngqqvih367";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "websec-diagnostics";
      versions = [
        {
          version = "0.1.0";
          hash = "1m0zqnnxm4ml00fc8s7w4f5flxrvxc6vivhcwmkgihvgmd8sha05";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "websec-lexer";
      versions = [
        {
          version = "0.1.0";
          hash = "1n7a9ki97absm1w0ild19apyi6sfh7pib4q69lxy72lfnifxvghz";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "websec-parser";
      versions = [
        {
          version = "0.1.0";
          hash = "1qgdinfspxsvc7ll0akxcp2y7r7lajcmf9c538zimrjvszlbafsa";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "websec-semantic";
      versions = [
        {
          version = "0.1.0";
          hash = "1x46mmhznrmcx49bq68niza02jw81hq8vnnqs2pa7nbsi3x9bhxy";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "weftik";
      versions = [
        {
          version = "0.0.1";
          hash = "09xgba80gh1blzvjcz67w9krwhx0a1davn7da4rgcmxiw0vx8cq2";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "which_dylib";
      versions = [
        {
          version = "0.1.1";
          hash = "0flxk1d9sk15kv7lv8vi0nipjrw0fbddraja44136d7xpqahzg82";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "yaml2cmd-build";
      versions = [
        {
          version = "0.1.1";
          hash = "1vci06a19y39vwjpvg6w53yfqrfvpqqfg2fgqkjzjncbk7wa4m87";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "ygopro_lflist_reader";
      versions = [
        {
          version = "0.1.0";
          hash = "1vda9fh8qjv29fiw4shxzxr7yzqpc877k68kjgnmy4rbi6588a8i";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "zakura-pir-enhance";
      versions = [
        {
          version = "0.0.0";
          hash = "1kkfk5bp2fbgfvvfmpgd5162a5jrkil2rjckmk8ins72a30pc9ss";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "zakura-pir-enhance-types";
      versions = [
        {
          version = "0.0.0";
          hash = "0rzx33d9zibvzb8h6bq37p495kgggaakqhasaqkw3ribgndkzqyv";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "zarpyon-support-transport-reqwest";
      versions = [
        {
          version = "0.0.1";
          hash = "0kkn7ijrklcgbrzggdd8wjw4hpskj4r60kxnddnxs3n7md1d0iis";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "zarpyon-support-webhook";
      versions = [
        {
          version = "0.0.1";
          hash = "1gwzknnw415hbagwgw2virj67pbfyfdw2qilavxnpgnlym97n9zb";
        }
      ];
    }
    {
      ecosystem = "crates.io";
      name = "zhc_langs_macro";
      versions = [
        {
          version = "0.0.0";
          hash = "038w0sn8vyqy8h6a3ngjxqyc9vl42sn9vij7ghld9a7l5v55z16j";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "aqz";
      versions = [
        {
          version = "0.0.0";
          hash = "1883q0yx427czr13gqjk37vk7wzlbgp33jash2n0bp8s32wpfp2m";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ar-border-predictor";
      versions = [
        {
          version = "1.0.0";
          hash = "1h0azrffd7mwv1wkamdw4j2l9mazc3b5z6mi6ygbrpxb68bfjvw1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ar-experience-builder";
      versions = [
        {
          version = "1.0.0";
          hash = "1hx9ix42qw1vj4qwiwpv171kc5hl1gil9w80s0f50m9ym7rjk3d5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ar-force";
      versions = [
        {
          version = "0.0.1";
          hash = "07g10kj9iaplip3lrjkl16m6yh7nnaasmr3f5cm1r1hv4im16306";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "azxcv1";
      versions = [
        {
          version = "1.0.0";
          hash = "1g8baa3hdcvk5r19w04slz4njx26p2ri8j08r2mciwsimiih33j8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "azxcvgfds-npm";
      versions = [
        {
          version = "1.0.1";
          hash = "0alr5fq4mvynhjy4ampf52yf58417yvnk2sgagr072yrv5lziyy2";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "azz-storage";
      versions = [
        {
          version = "0.0.4";
          hash = "062inczg07b4y0k2iwyn8djmniij0f0hhdabbpsb7k938kcviwls";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "b-check-balance";
      versions = [
        {
          version = "1.0.0";
          hash = "0rrdjfal5b2wvq0pk5bc8f6hi7s58f0adff6f8ir300m2kmapzjr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dqz0a5";
      versions = [
        {
          version = "2.3.7";
          hash = "153kw6bbv11qz90zflkpcg597ksmxijlqq29l8af7y9vg1vj2nka";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dqzhiyu-tcb-base";
      versions = [
        {
          version = "1.0.5";
          hash = "1kfwnjlpyd9y2kx4misyjaxq2327snr0prikk3mnr98crmi59n5r";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dqzvgxuatoadream";
      versions = [
        {
          version = "1.0.0";
          hash = "0wb20gfm01nx1rrq9fd9626ac55yn2injsnb9mc89swv29nx32r6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dr-dev-babel";
      versions = [
        {
          version = "0.0.6";
          hash = "14kpizqr8k2l30yc325d88752nh7ajb343ws6qcczd0hxc9byxp4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dr-elephant";
      versions = [
        {
          version = "6.6.6";
          hash = "18b3a4mslq8a9sbpyjajn48bf1nb348w42n6h9z4ddd9b0bvyv9q";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dr-hooks";
      versions = [
        {
          version = "1.0.1";
          hash = "130bf2jg418pvizhs6f75vjc1m553ra6m2zmc7vng351zskqhl3d";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jzz-synth-tiny";
      versions = [
        {
          version = "1.4.4";
          hash = "1zc71pxr3nhrqghc80671k6n6yj6488w33gbx75arqzf17bagqdz";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dzxcgsaz";
      versions = [
        {
          version = "1.0.0";
          hash = "0kvgy6xrvrr694g98g0fhiz06vs8wszn87ds834lxikgla4fhswm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dzy_utils_01";
      versions = [
        {
          version = "1.0.0";
          hash = "19kcq88cpsl5if9pl4zxmbr5h1jxmvw7r52md1fvcqi2q467n7cf";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dzz";
      versions = [
        {
          version = "0.0.0-alpha.0";
          hash = "0jf7wkhmcj5lcj0zcj8dpn6ms9i6nrwcavbpbkwc7rvla2bfgyn9";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dzz-components";
      versions = [
        {
          version = "1.0.0";
          hash = "1la0xxxdc98zw3dgabs0y8sbq7c6nw4v82c7cyan6h0ndbq0gcs8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "dzzgd-library";
      versions = [
        {
          version = "1.0.1";
          hash = "1lhlpsz0kc4alfskvbs4ksl1bmicvqhir32wdfir190a3bwr13fk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "e-async-control";
      versions = [
        {
          version = "1.0.5";
          hash = "1akskzn3wh47g0lr5w2d4kfdd6k0kx4c8s2yh9m9g3qvj91qqs2r";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "e-boxes";
      versions = [
        {
          version = "0.0.2";
          hash = "0fg45xkms9pbycda27sf6q9xxik0k8im2bf57xal5bfjah9fhh8d";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "er-det-fredag";
      versions = [
        {
          version = "1.0.1";
          hash = "0k32pif14f018bgbc2rc8k1pb4x8j5kfd6vbpjqf73zdhdnsn7vl";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "er.js";
      versions = [
        {
          version = "1.0.0";
          hash = "0xdh09hxa715ll6s87i5ygqqf4yfmgim54pxb4rzalgd5ld3czll";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "er26o3";
      versions = [
        {
          version = "2.3.9";
          hash = "03q3d3qx6x7lman0913xrfmvbxwmwrxp7vdr6wk0v8pq16wicw24";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezy-cli";
      versions = [
        {
          version = "1.0.30-alpha.0";
          hash = "1wqwhzicw8lq9rri43kan84xg84fhigzbndc3xrs235jy8jlh5si";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezync";
      versions = [
        {
          version = "1.0.0";
          hash = "1lnszhlgy2fvxx8h88r1hi4jhz7m3jim5zqnnf99xf2x7wk244z2";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezz";
      versions = [
        {
          version = "1.0.0";
          hash = "11sw2ibgrs2j63ip8pdyhc0fskds5mj5bk4b9lm4i70j3832w0vq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezza15";
      versions = [
        {
          version = "1.0.0";
          hash = "0g87q1yys9cc4crvrj1n864bnpj3ga3sy1zmhzsrahifp33c2vm4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzip";
      versions = [
        {
          version = "1.1.0";
          hash = "11mq53kpj6i9yz0jldj75wnrcxbkh74rllirmia3av4cmfmpb6q0";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzy-argument";
      versions = [
        {
          version = "3.1.5";
          hash = "1qz9pnnbg3mvvdyywhss150cvnfk48a0jfmgmqchmrps1w6904kb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzy-express-basics";
      versions = [
        {
          version = "3.1.5";
          hash = "1ij4rsbbdr17fflj46dj2akc0px4bjly33bwkxfs88hb5nafrrms";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzy-ip-info";
      versions = [
        {
          version = "3.1.5";
          hash = "14qqfwczwikihbdhkw1xg854y0skj353a5i2ciwrlqgk299pva79";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzy-package-loader";
      versions = [
        {
          version = "1.2.0";
          hash = "0nlf2yz5p1z1iswm24398vf37fphgy1kidfnwv9yhlsm7qzk0y0x";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ezzy-web-crypto-hery";
      versions = [
        {
          version = "1.0.1";
          hash = "1ffac9pd6zrjh9w2692dbbqg8nap3qfm0y7gc0s13463b8812dxh";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-04a-test2";
      versions = [
        {
          version = "1.0.0";
          hash = "0rbr6z597nxxrzyl7626jja9pfyj3ghp44wgshz1bpbrdykc2a41";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-jwt";
      versions = [
        {
          version = "2.0.0";
          hash = "0ywsbsvbnmiqjak3588mhkn6b8qhwv0cz1wip9qjrwywkpp389lb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-md-ec";
      versions = [
        {
          version = "0.0.5";
          hash = "19zhjmsrb9lgjjwss694asfdvbczj25zci8zc9q2i48x0kn9bwgb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-md-hk";
      versions = [
        {
          version = "2.1.20";
          hash = "1ipgrzr77wkh6hw066zndg6vhz1ml11rmc85nly2l6zddhdjpzx6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-md-lts-hk";
      versions = [
        {
          version = "1.2.7";
          hash = "1ama5yf76lyh7rp04md43c8pa0q4nfm543ng7x0hqrfwc5nsmbp0";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-mnky-my-math";
      versions = [
        {
          version = "1.2.0";
          hash = "0l7d0hpzb78hlizzmb89m77j7ha5ln2ygf101k09av99v26xwy79";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-random-number";
      versions = [
        {
          version = "1.0.0";
          hash = "08bphgiipln8xc6s9i6iaw1qgqwkpza791nbfbspp4mpdlqigg38";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "fr-react-simple-captcha";
      versions = [
        {
          version = "1.0.0";
          hash = "0p921d7dmw3q8q38il4cb7amspcbx2452aafbb3vrpwpifqlv0lx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-audio";
      versions = [
        {
          version = "0.0.4";
          hash = "0ndszay21m6jda9bni1hvk5d62wxi9xx4rk27126fwps8h399zb4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "g-5-npm-package-1";
      versions = [
        {
          version = "1.0.1";
          hash = "1f3nd9iv3777n7wc5xh3y024cii2837vbl9yq7sbqqnjsvyz478k";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "g-apply";
      versions = [
        {
          version = "0.0.2";
          hash = "10symihzp7rddwk58clw076zi2mj40rpgvgvzb42haknjnkcllbd";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "g-atom-css";
      versions = [
        {
          version = "1.4.0";
          hash = "003fj0kkijqrkfwqbf8aj2940bb7m70v705ahqvsfcb8cpq4nqx1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "g-bardai";
      versions = [
        {
          version = "1.0.4-beta";
          hash = "1rxf4igdh18rhikglagkb9cn33dpmadnisn28mk074k92wh80a00";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "g-color-picker";
      versions = [
        {
          version = "1.0.2";
          hash = "0c9lxi354pmcc2y5fb811vqh7hk2bamfpi6h4rpbkc50z3p4vdjr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gr-bin";
      versions = [
        {
          version = "0.2.1";
          hash = "0m3k7n9g2ffwd7jyac9i6spdz3czdwald9ffbyhs01hw303p7mnp";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gr-iut-encrypt";
      versions = [
        {
          version = "1.0.2";
          hash = "0bwq6x6nd7a64kja7alppalm7zfqg2wkfrdky1pdhdx1i9n02iah";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gr-tools";
      versions = [
        {
          version = "17.0.1";
          hash = "0xs0al8fx35rb1lfnd5wrl2blacyc7viipc8xhyybslgbmp5ngc6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gr-util";
      versions = [
        {
          version = "1.0.1";
          hash = "1fa9fhi4r7nh6i2z1pfjcska1isgrcijp8xlcf6jvk8a1227574f";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gzx-dist";
      versions = [
        {
          version = "1.0.0";
          hash = "0yl6bynn2g4nnb9p4axwga7l27d6vj6zg5w13wh0d9v8vx9yqpcx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gzx-muk-ui";
      versions = [
        {
          version = "0.0.1";
          hash = "0jsc80a8c9x9rvhnzqbi9ygayjc9isc8lxvscipib0p688fi1rn4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gzx-proxy";
      versions = [
        {
          version = "1.0.0";
          hash = "032q9ln7wkibxbrmq847qqzvvxv9x081zmwwdcm8ksi38anvscf1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gzxljjgzxiop_123";
      versions = [
        {
          version = "1.0.0";
          hash = "0zpyir1nljrrbv56x1z1rzps5yxmnzgnad0sc0rz2993zr32gb4i";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "gzyzwx-utils";
      versions = [
        {
          version = "1.0.0";
          hash = "0vhhr95mxidxwhiihq4zx7lzq3sj1ms4ynqn7nl0agwb2c1ac7q8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "h";
      versions = [
        {
          version = "1.0.0";
          hash = "0b0112k4fxfz9nlc63fg1l7dd6ghp1pskc3qzcf29h5iiqbrwxsk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "h-1109-my-test";
      versions = [
        {
          version = "1.0.0";
          hash = "0nd0alrwr71n2s341r048y1r6qvnhycq87wvhx526g44zda8h6wk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "h-119999";
      versions = [
        {
          version = "1.0.0";
          hash = "183ib5phmyh9gr8a6qiais2zhn5wxgwym216v7l32s78sk48gg6r";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-br-component";
      versions = [
        {
          version = "1.2.3";
          hash = "12h008qmkqx5axhgxna14ar3qddylcdmyny2knskvyqqby4m1r42";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-bytes";
      versions = [
        {
          version = "1.0.0";
          hash = "15g8lz8i60jdxk88s4fgbv5mvbzrbl626kmk23109hvmfm8nzi3h";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-calc";
      versions = [
        {
          version = "1.0.0";
          hash = "1kh8pmhlvvyxz9dvnfym699cc7vc92jyj0jjlhvgvg9p3d7cf1xk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-calc-using-inquirer";
      versions = [
        {
          version = "1.0.0";
          hash = "1hic6n7p5rljjqyix2kqcz4jwbimnka1p9hpkssivdsxsffasrsh";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-common";
      versions = [
        {
          version = "1.0.0";
          hash = "0hljq90khsyz9v7ym54q9ff52nk8j1jcd4nx4jv8648pval6blrg";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-modal";
      versions = [
        {
          version = "1.0.6";
          hash = "08jcgcnm83ar80rna3fi5lwm6jsx40fj42azvcqllxbw91qqkdsj";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-number-guessing-game";
      versions = [
        {
          version = "1.0.0";
          hash = "1pbw126a5gabfbl9kr93fcw0ymk7zgia1w6ksayndfnsdag7g68s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-portal-frontend";
      versions = [
        {
          version = "0.0.0-use.local";
          hash = "12amlbiqgnqf6kmrbi6pyl6pgahbamb1q01wsfd45fw7x1k1l9h9";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-proto";
      versions = [
        {
          version = "1.0.0";
          hash = "0vc158f83nbflad6hvj4gvwvqdv970l32vyygkx0xyhgcgqjcay8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hr-resize";
      versions = [
        {
          version = "0.0.1";
          hash = "1m2mqdsypqcsydj48qambblvhnzfq9gwyzmvh2in3f7cl49rq4as";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzx_get_json";
      versions = [
        {
          version = "1.0.0";
          hash = "0m4z6wsmv1lw57vpc0a35pgwil03978dpn66488r6wkcm595nm19";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzy1205";
      versions = [
        {
          version = "1.0.8";
          hash = "0isrjpzjagd6sjhmn45qivfsnrjrv74c4s4nggp1n7d2myl96sc9";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzyx-demo1";
      versions = [
        {
          version = "1.0.0";
          hash = "0sacc4qf331bc0a1aapb7xg51ls30py1502ac49a0lr65gzl3fba";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzz-npm-test";
      versions = [
        {
          version = "2.0.1";
          hash = "1qapmvn0ikipg06l6mbdd733r8l35p9mv1hs61hwpy1xhm1ss74q";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzzcjdkoadenets";
      versions = [
        {
          version = "1.0.0";
          hash = "1kvpilmmq333rrqbc4sl3nnkz4w1wc5cmbi19m6ym0kfdk27n4h0";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "hzzt-ui";
      versions = [
        {
          version = "0.0.1";
          hash = "0hb9a7brv3gp91gx020j0i8fhjnakn194c3y78mmyv9vy6sc2fzy";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "u-cache-ui";
      versions = [
        {
          version = "2.0.8";
          hash = "1g183y55an63jm5h12di5hlr64f5sd766hyl22ggf4m1py6pqxp5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ir-restifier";
      versions = [
        {
          version = "0.1.0-alpha.0";
          hash = "03cd38krs8rmnayhzapzcfl010wgakcfc9k3pp9ql9zwhpaz8srm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ir.cafebazi.list-controller";
      versions = [
        {
          version = "1.0.1";
          hash = "0yr1h11kc3gm7k4jv9wz80j43fwcv9kf24sz48ayrpx25lh0paff";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-10";
      versions = [
        {
          version = "1.0.0";
          hash = "04d4fl1q3pw5l2r4s3z4cslnv1pglzqmysxy032gzx4iazfq4x21";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-14";
      versions = [
        {
          version = "1.0.0";
          hash = "1166b2jx5d7kw50hfg01hxlgq67ds837yc52lknnaycr6qyhw8jc";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-15";
      versions = [
        {
          version = "1.0.0";
          hash = "128hy4zbw3jl7jzdhsy10bhmy8irr5jqvwyma1wdy42d1ims3zgx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-19";
      versions = [
        {
          version = "1.0.0";
          hash = "0w9179m9905a2hqd65502qlj15xclhcvld31vc49j53jh8pbjvav";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-24";
      versions = [
        {
          version = "1.0.0";
          hash = "0yc7xh1pdwc184imzgb446bblhsrzr466mvmajxlksaigxgh0yga";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-28";
      versions = [
        {
          version = "1.0.0";
          hash = "03rxmcqyfkhz93q1r5zmjjsrv2b3c12dghbf991905rzf0l9nhjl";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-30";
      versions = [
        {
          version = "1.0.0";
          hash = "1l9azvy61955y2r5lipsarm6whb0ya02c2xknwhbn2v1l3cgrhd3";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-31";
      versions = [
        {
          version = "1.0.0";
          hash = "15h5p0jmmq4k7i8lblpmci62racwkn51r08p120843qd7alfl82s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-32";
      versions = [
        {
          version = "1.0.0";
          hash = "01zg3xjgiha92jqrpydi5hpnxyvlapqf22s67v3ffl5gnb6l37hn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ira-39";
      versions = [
        {
          version = "1.0.0";
          hash = "0hi2d12sv0lp6z82f0a9bhi4km2q7bdlxn0qm46yd1hyk5zrm152";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "izyware-dataconsole-elasticsearch";
      versions = [
        {
          version = "1.0.4";
          hash = "1nly94whk3njmpxqlq5ncb32sc37zlz552rfkchhz8lmjvxic8dl";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "izz-lib";
      versions = [
        {
          version = "1.0.0";
          hash = "193vkhnk5n8fblwhrp6ya2cxk8lya4zvk5m7cxdd79j546v2g2a6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "izz-test-lib";
      versions = [
        {
          version = "1.1.0";
          hash = "1mnlr4mskcgksfy2irz4nibwcsmyfil5jxjwf941qa856s8wlp75";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "izziek86-fr-pr";
      versions = [
        {
          version = "1.0.3";
          hash = "05wz6jb8qdp9gp4isc5fwqsmycnchkavz930l5g473wkfxc9bpm1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "j-2024-cli";
      versions = [
        {
          version = "1.0.1";
          hash = "12zwnr5j3574hzn20rscjhkb1idn847sq7jp6pjgak9ql9y1b6kj";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "j-adapter-craft";
      versions = [
        {
          version = "1.0.0";
          hash = "1198h61s8f2rb1vi101r2x64mlkn412xh62qn5mzkk6rgix3zlyr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "j-bare";
      versions = [
        {
          version = "0.0.0";
          hash = "0pjciyszcyly53k7mvp87vbp7sl0xlx63pmngkv4jfjchgq0q594";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jr-browser-storage";
      versions = [
        {
          version = "1.0.0";
          hash = "09nffclwh8v487dvglp918pqwn62snnkr13pmjyb5ffcid0k33d7";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jr-callbacks";
      versions = [
        {
          version = "0.0.2";
          hash = "15i8a67nh8srp08cjhq0avz23rj4j4kag0h4x21qvprf1aygqpqb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jr-ds-components";
      versions = [
        {
          version = "1.0.1";
          hash = "0qf934rjz1bgl1qw7fasyd5iziy1qawvxjsrh4apkwmmki0krjx0";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-cmd";
      versions = [
        {
          version = "0.0.13";
          hash = "06wjc6dszrq5s9pgwrvgwyv6154jbfk1h2rnrhw2annnn91im4g2";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jr-test-cli";
      versions = [
        {
          version = "1.0.0";
          hash = "0s15k5ns72c3iydqa8jb8s8gy40li0ji6mbdsy0mnybjkr8c2g66";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jzy-mini-program";
      versions = [
        {
          version = "0.0.10";
          hash = "0ql19pcgs8479ac2bflh7ms0qacaqwxjrgyrmwdxsr1mlifzinkk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jzz-controler";
      versions = [
        {
          version = "0.1.1";
          hash = "121iva7k2h04c7nx8bfqyf4ny65q7wvabwanyl6673cxz2p5icmf";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "jzz-input-buttons";
      versions = [
        {
          version = "0.0.2";
          hash = "10w25adchmsdcz1k1ki0f2b3nfhlp85ss7dyjv6nriq92869mnns";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "k-arena-mcp";
      versions = [
        {
          version = "1.1.0";
          hash = "1g4yy966gmn7ygl3x8cyacshhl5saksjikif0w80vcby5y2nmfkb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "k-boom-components";
      versions = [
        {
          version = "1.0.6";
          hash = "0k0bi5yixyiw2fkhv669p5vnlpfpiiqlbksm6a0kglv5km3wv9s5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "k-button";
      versions = [
        {
          version = "1.0.0";
          hash = "19swqg4asw73bi1d43h94ksr0b3wvs1gmnj3cncmbl2q4b1z06ha";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kr-form-validator";
      versions = [
        {
          version = "1.0.1";
          hash = "11q0byxmh482z9ahigd5iqcwsqhadz10qgmw9vkysmkgg75pibzk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-06b606cf";
      versions = [
        {
          version = "1.0.0";
          hash = "192yfhdrf1fmlw65003rdsmazclnn9v2yrahnsbr9wfikdijcmhi";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-0c931d10";
      versions = [
        {
          version = "1.0.0";
          hash = "0mm8i91bvc6znz5aw0pf28bw4n87xy7pf47j824fnqaj0dz90dih";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-1c0b39d4";
      versions = [
        {
          version = "1.0.0";
          hash = "1r9f6b0wms3mscsp49b563mcqwrmx71l70m2f641rrqhfl3dflq7";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-24f9180b";
      versions = [
        {
          version = "1.0.0";
          hash = "1p2jbbgqzm6cymc19ckhk7ndnyqrpcq252591swz0r6j6hfw71sl";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-38c4f59b";
      versions = [
        {
          version = "1.0.0";
          hash = "1h5hkrlxkx2dx8gvywqpnvnv0fjh6w1f22n8jss8jrqxrw1kyfrd";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-3e633616";
      versions = [
        {
          version = "1.0.0";
          hash = "18d548l5xai2kkvjhw0b21dp2sc1hi0s6anmbq3d7wcc4wqciwqp";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "kzx-3fb76eb9";
      versions = [
        {
          version = "1.0.0";
          hash = "0vajjh7bdhs9pdvzbx177xfc37ig6n2kchizw257f0wzkjz22k38";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lr-test";
      versions = [
        {
          version = "0.1.0";
          hash = "1n81i2019sgyhx19hhv27siap52vrry8iva2b27ibfc7z04jwfjb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lr-uk-dpl-p2p-velikobritan-rus-wnu3cufmr";
      versions = [
        {
          version = "1.0.0";
          hash = "02nvwkqlbv5nmdzfd2brhav8ivryjyqk30cvzx7mf4126f1jb4vq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lr-vivadengi-ru-p2p-moscow-rus-gom7c1rxk";
      versions = [
        {
          version = "1.0.0";
          hash = "1258g68lhl34fqhfx047wii53f8k7mfs91lcwp9j47d6zvq73x06";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lr2lgx";
      versions = [
        {
          version = "1.3.9";
          hash = "0abjz16z4fzfvm1cwcjqpwk0a9ip6j38xdkn17xrrj6x94cy1fiy";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lzx218";
      versions = [
        {
          version = "1.0.1";
          hash = "0pq1snk79k2il3hfvy3m0sr0mmdp9qf9k583jr9bdw45mqfnwfrx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lzxx";
      versions = [
        {
          version = "1.0.0";
          hash = "1kdiwfjf0bhaz1lnpl4k17wkmv0zzhgvsvlbc0x2qxda6y2criqg";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "lzxx_jisuban";
      versions = [
        {
          version = "1.0.0";
          hash = "1x05aahn5skp98g9v0x994ycags3d0am0p8sp87m0pyr86qhcxr8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "or-sample";
      versions = [
        {
          version = "1.4.0";
          hash = "09y3wri6rvamm4ca7iipgwx897qi7dmx32vwwskkc4mirmaa4fkk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "or-schema";
      versions = [
        {
          version = "1.0.1";
          hash = "1n6j919lcda011b1f90203316af3zn4vwqjz68pq2fnis5c1jrjm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "or-test-pkg";
      versions = [
        {
          version = "1.0.27";
          hash = "0iffwmjz0r9kawndrrd54b3r9h86vs85jy4z8xk8ly7p2s805awp";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "or-theme";
      versions = [
        {
          version = "1.3.0";
          hash = "07n7h9psfvs6fyg4kbrp88mj56119bxpcrsv9x44pjx90rw2lmwd";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "v-api-docs";
      versions = [
        {
          version = "1.0.1";
          hash = "1lavmr7jqyi81pji5z666l9dn1zb542fc6x1azm80rz7z2m120hr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-api-log";
      versions = [
        {
          version = "0.0.4";
          hash = "10gi9ssh7adki8hvdrr7f04b042x1c4xi7dp92p339jmb9y1af5k";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-batcher";
      versions = [
        {
          version = "0.0.1";
          hash = "1dw39mc30g1kvqrf4r5kfcp6g8i8ky9kjb40aqgn1yz43mjx2m2a";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-beidao-test";
      versions = [
        {
          version = "2.0.3";
          hash = "15wzjf3vim8dsd0ll399wglyc0534vkab9fgxb9w0frqli2hy77z";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-break";
      versions = [
        {
          version = "2.0.0";
          hash = "05b9b0sz1r10jqpmc1cv6nsfln1fwzhz5y7kgskibxpb2wy3m5cr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "p-cache";
      versions = [
        {
          version = "1.0.1";
          hash = "0mcavazjcwa0nmarvfmkfwzxah6mkxdp2q51lrzsax6fwcf5sl06";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-6-word-counter-app";
      versions = [
        {
          version = "1.0.0";
          hash = "1mi2ghi61diajmcp1drnj1i8jnhcj50lyii8swhzdcvp46nbmkr5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-arena";
      versions = [
        {
          version = "0.0.0";
          hash = "07hk95mwvhvhiia9q4d5150lb79lr2pkzzj42fnmbsvigc671sfw";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-chatapp";
      versions = [
        {
          version = "0.1.3";
          hash = "1w2ia72n0zb69k7jqwglcijk70wnykyc03nyc88hr364dylvwp6k";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-conductor";
      versions = [
        {
          version = "1.2.0";
          hash = "0iqghqswb4ys384m0h39kmzh14s6khk1hmki8ghmfqwrdqygrs95";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-currency-converter";
      versions = [
        {
          version = "1.0.0";
          hash = "0gpjnjx8jzlpjgbn2qqkyc1rja68dkv96ni7sijgpnml3hm17nrq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pr-diff-validator";
      versions = [
        {
          version = "0.2.4";
          hash = "0y6xl93in8i2wablh1s6637cjvk2jdp3sa6as4r0kh6k4ii776n6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pzy";
      versions = [
        {
          version = "1.0.1";
          hash = "07sf59vscncsjsrzhb4l2c05qc4ayh4a7ja10lvs9vv6rvsbmbp9";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "pzz-demo-pages";
      versions = [
        {
          version = "0.2.0";
          hash = "06b42arjliv1j68wbwr7xg4crzvljizwnncmh8yznd0rj7asgqkk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-backtop";
      versions = [
        {
          version = "2.0.0";
          hash = "19qmxxx2gxxh0xsdmhjl4lyx0b886lpblvfkbawaqpsl6d6n9fmr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-base-web-module";
      versions = [
        {
          version = "0.1.1";
          hash = "06ds6nanrh9mm0vlb2f1lhsw0r5nr76356wl5z9mypyrx9s75vmc";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-bot-transcript";
      versions = [
        {
          version = "1.2.42";
          hash = "1qba5wygn167fc8f9v1vpgbp9yd6bmsbby3vccs34pvs2g7xqr3n";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "q-create-form";
      versions = [
        {
          version = "0.0.1";
          hash = "0ybn62sanyvm55rn621dc142a91v3vk1yy657mvmv07y0d9ng7ky";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qr-cli";
      versions = [
        {
          version = "1.1.1";
          hash = "0kg91y00zap926pwwj08j57yvzc85xmqnwkdi56hbi9flibj1jdb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qr-code-encryption";
      versions = [
        {
          version = "1.0.12";
          hash = "16y839l40r9d94lkxqgr8rvv7b0q49xvv91klbf1ll6gdj1xxap4";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qr-code-mcp";
      versions = [
        {
          version = "1.0.0";
          hash = "1jxna24licp7l4xcmm063jaraz9fx5c0c2k2kr6pb1yah4fg2138";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qr-code-nice";
      versions = [
        {
          version = "1.0.2";
          hash = "1lyirx0w80l81i48w7hh6l8fk0wp44vf55pqq5g8jczw4zghy804";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzx-decorator";
      versions = [
        {
          version = "1.0.83";
          hash = "1l720xx8sb9ryw1avh7i91191xxiilv4qamyaggjxr5y7rvmjy7m";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzx-encoding";
      versions = [
        {
          version = "1.0.0";
          hash = "10jmfzsb4wv2c73kf0mwmp4gv96224cagp0v97klc32i6y1vyf29";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzx-host-tool";
      versions = [
        {
          version = "1.0.3";
          hash = "0ahr8b3bfl2k4xm4dzss15rb8szrxkbxsn57abn6swx5c9jhqwvr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzx-view";
      versions = [
        {
          version = "1.0.5";
          hash = "0383pbcrma3kri20q09pxzzhkzcl0ad3dird9vssi5n3fzna1jy6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzxc4578";
      versions = [
        {
          version = "1.0.0";
          hash = "1sla5jx39gqrn5gmkh0i5jxw3z3ziy879z192frkv2ljq7yhw8li";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzy-npm-test-winstin";
      versions = [
        {
          version = "1.0.1";
          hash = "0s2prnb9kwa2pal91awahi1wqzp9xsi9fb9lyg7izsjab5bbm452";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzz-test-vue";
      versions = [
        {
          version = "1.0.0";
          hash = "08lh9w6jq2sn3zw0yhfb2cgm0jk8sgrfcagyqfm68005k5fd177n";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "qzzupload";
      versions = [
        {
          version = "1.0.5";
          hash = "1zg32p3r0sr8r2y68x4n6z9698zwwa9mzjp5nhhmsm82429ii8fn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "r-123-currencyconverterporject";
      versions = [
        {
          version = "1.0.0";
          hash = "1scyvr9ani8vfb82zx0kqkcln8g74dbbshc809wcq0w3nw1mm19x";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "r-2-sonar";
      versions = [
        {
          version = "1.1.4";
          hash = "0ng2r1ffwyjmyvab7n8i3mrxs4af5rx3hzfpwwqcm7c4z934pdl8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "r-60-idb";
      versions = [
        {
          version = "1.0.0";
          hash = "0nq9cmgigk6wrmg88kr0gvqgrj5wx01x4zd6cvv78arbqcyacll8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "r-accordion";
      versions = [
        {
          version = "0.0.16";
          hash = "0svqkfzy9m4xm4qigcc1jkr81ks8z8s10m6f9vz5dmyrnydrq0w8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "r-ajaxhook";
      versions = [
        {
          version = "1.0.1";
          hash = "1nmfgl3hppyj70mckwzfw6kwdnbsqq7xmzv9dqsz6aad2m97y4is";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rqz";
      versions = [
        {
          version = "0.0.0-alpha.0";
          hash = "0kf0a64kfkm5nc69ir00vbf72034w7jycfwhdhr92nmk0r71qfxw";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-base";
      versions = [
        {
          version = "0.0.0";
          hash = "0gs4h19drs3cs5fjscvm9g56wpqmidanwmk2ikdwsv18njmf38ma";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-board";
      versions = [
        {
          version = "1.0.0";
          hash = "182nwd1rh3l26ycj3nagc0x45bw5g9jp9z3f8dkzcpqabyyhhm88";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-cdn";
      versions = [
        {
          version = "2.0.0";
          hash = "0hk5y89vziynqr5vhcgn06sqf34jlbqd55hfbw300zkzxibf1afb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-dev-components";
      versions = [
        {
          version = "1.0.0";
          hash = "0d7n670dybp0d6dxblf1k1kxr03wmj1wh11l3a3vakg6is3kl5wm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-link";
      versions = [
        {
          version = "0.0.3";
          hash = "053n7775a8v087z8hh7sbz5zchp3abkffisw5zgpagpn09fqbm04";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-package";
      versions = [
        {
          version = "1.17.9";
          hash = "1wyi7wywcq9b8rzmd3qnn66vpwkfibnlsqc7ij32wynbvbkpz5ds";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-steps";
      versions = [
        {
          version = "0.0.3";
          hash = "1kn5bqxjwzch6g9aw4l8i7zaqmdzmm40ajnk88s3mivqrk2hvbxr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rr-test-lib";
      versions = [
        {
          version = "1.0.3";
          hash = "1zrxsh8b809mcpk4wh5z6qimbnj6rnh8i5dsgph23s8rnsgfdp0i";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "rzxx-ui";
      versions = [
        {
          version = "1.0.0";
          hash = "0i5gpdwvm8l3z3b5xq8h6b5p04nhgl01hnpa9yxqmi95q9x11q5s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "s-32-new";
      versions = [
        {
          version = "1.0.0";
          hash = "16r7c5lz336m7rx1sry9zxqim4q424iw9p585wlz2bv7nvzgx1jp";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "s-apps";
      versions = [
        {
          version = "0.0.0";
          hash = "1nxj0rq84ck8xsjkrnpkv75vsc6ml5mb24ks83mgwzygrc2wabh5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "s-c-simple-calculater";
      versions = [
        {
          version = "1.0.2";
          hash = "1lpckf9pinn47zwdxsr4avq3bghjmrqdqgn3g2ph9xw8ic6kaw4p";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "sqzygjyujh";
      versions = [
        {
          version = "1.0.0";
          hash = "1vspi9b6dcwdmw25pvb0gjvignbdiyf9kwq6ichr0228g02bg710";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "sr-1804-a";
      versions = [
        {
          version = "1.0.1";
          hash = "0w9g1p16cyd2xs30360d9i8d3npch89vcbyi2ashq8ma4np2vjys";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "sr-cardvalidator";
      versions = [
        {
          version = "1.0.0";
          hash = "1phwappfrlh22rxr8k59hy96aw3hal5yqgkh2wsf8p778ppi1w00";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "sr-cos";
      versions = [
        {
          version = "1.0.2";
          hash = "0gyyzbwavbl2g2fjmz72njzjkhfgi8l8xs7p1nss5k4zhsivfrh1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "szxtypeof";
      versions = [
        {
          version = "1.0.0";
          hash = "1qsl8h3dg05qj9jjj3p9d9shskm7wks8dd841idmsbxw2xpcb2gv";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "szy-design";
      versions = [
        {
          version = "1.0.0";
          hash = "0ic95plhcik362nszv0vwa8kj5qjmxy40c2nz1a9ajkm6zld6qmq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "szy-throttle";
      versions = [
        {
          version = "1.0.0";
          hash = "0gs0s0lmqzmvq2085qfrhh96rs7q3pnyramni27wg6ish0p9k05b";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "szyh-columnset-handledata";
      versions = [
        {
          version = "1.0.5";
          hash = "1b01s0basj8g7l2djrfrswwpzd4f8ghynj4hnwlddl9z5cb1997v";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "szz_demo";
      versions = [
        {
          version = "1.0.3";
          hash = "0yvn60wd07kxfk4b1bm2yxbvqjzqgg7b9cz8jh8ddsbz1l30gllm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-ble-sdk";
      versions = [
        {
          version = "0.2.0";
          hash = "0vbwdis9436j18hhz0xj43fm5fzvj4n3p2610s7ch5s14kkqc2wx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-button-component";
      versions = [
        {
          version = "1.0.1";
          hash = "175zb4b1fvr8rf0swmf1y583fnn5h5p4cajlq2ra55m3ninbk7rf";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-button-test5";
      versions = [
        {
          version = "1.0.0";
          hash = "0s7h6djb50davnixryvkdzw5fggg2qrd7006kkc9fw8kffbg5isw";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-date";
      versions = [
        {
          version = "2.1.1";
          hash = "0z1fwsn3vbafisv7gvhg6v9qkjd5nd6ybpfhlb96b1rp0b31ni1d";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-doviz";
      versions = [
        {
          version = "1.3.0";
          hash = "1clxgwhlwbvlbw1a6b62r9ds9gm6flv0r4y5k6aq6zc5831x2f45";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tr-iban-validator";
      versions = [
        {
          version = "1.0.1";
          hash = "1qkymxgfr62805nhncr2b61dqgvw6s5k76450j80qxfzcjabfvpl";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tzx";
      versions = [
        {
          version = "0.2.1";
          hash = "1452dnymfqp76d9c2c5jmrk4z2c34kkzbn2jj68i4nqw082kizbv";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tzy-content-funtction";
      versions = [
        {
          version = "1.0.0";
          hash = "0jn3gv6w861ds7kb7ax693di6j24ns77lh1ps6hc5zzwwlr7zf0s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "tzypan";
      versions = [
        {
          version = "0.0.2";
          hash = "09d2yw5sf2k26d99cg6w8ibd1q0hzb22fhs579i1c4df45cqzdf6";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "u-86-number_guessing_game";
      versions = [
        {
          version = "1.0.9";
          hash = "1a3rsf11nmvabjskvyqpr9v3318dq28n412s0n2lksfj2k28ckdd";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "u-components-test";
      versions = [
        {
          version = "0.0.2";
          hash = "09fwy1a4mwhb9d8l55h58kcbvm4p1bigzvvppfvd5lrqh05clfhn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uqz663_lvw";
      versions = [
        {
          version = "1.0.0";
          hash = "0js2z60i6vf0rxna96s5sg8pmn3qfrblig28fc8sbi1vwhphprgr";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur-express-logger";
      versions = [
        {
          version = "0.2.2";
          hash = "1iar4b70xs3gca0ck60cw1z6njxhd4f44z9z5kzcap6kqfxad6gq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur-modal";
      versions = [
        {
          version = "1.0.2";
          hash = "0740zfnc5bkyk18zrmax90ignxspx30fhzqfjvk3yzkqk61mwchb";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur-netlify-function-base";
      versions = [
        {
          version = "1.0.1";
          hash = "1mynhcaj3fcdy50j3w1dwix0sd7dyy1ybsj7pky298w5zncqhrd2";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur-uuid";
      versions = [
        {
          version = "1.0.2";
          hash = "0xiby25si6hn6d3n2fccl4bwjd0qpy616lxcsw61ymcf2g9mh35a";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur30-word_counter";
      versions = [
        {
          version = "1.0.0";
          hash = "182299y2f1girn7fbh6vihj57q2km9yddhrmnz5pbsc2sqa9xscq";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur86j-calculator";
      versions = [
        {
          version = "1.0.1";
          hash = "0y4x73b6011q05ijhx0gb5ksndxxh9c1rqj4x88wr1v0ry2rgh2r";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ur_word_counter";
      versions = [
        {
          version = "1.0.0";
          hash = "015mqivwfm5h4m9pjsxy5766y5k1sw2hdfyp8wv48r7rsg0mnbhn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ura-prn";
      versions = [
        {
          version = "1.0.0";
          hash = "1p62ql8placp6w9x915y3xs576rdknf5i3y3ly4zd7mngrjh0dys";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "ura1020-ulib";
      versions = [
        {
          version = "0.0.1";
          hash = "17bh0dmgw946c2zxba7f78l48ckgwb06kac3rcfk6kfw7ms6r710";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uragifa-afaoti-uto";
      versions = [
        {
          version = "1.1.2";
          hash = "121bd4mfq05mpks1i6agp7wk4vi65j5aq08id1cdlf0m3nqslhzg";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wr-server-cli";
      versions = [
        {
          version = "1.2.1";
          hash = "0d8j5jr682q2p9s9gcngm6c3dfn62bv32cxivcay2vraqkd776bm";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "izyware-sqlconsole-rekey";
      versions = [
        {
          version = "1.0.8";
          hash = "0pi1b38mlrsr4kf4vad1fddls4l24zvyp746l690lmfskcahg6zd";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uzy-ip-uri-tool";
      versions = [
        {
          version = "1.0.3";
          hash = "1cygydp20m8v8srfadis8ndfa0z4si02iwdayqld269sy7a5lxi8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uzz4";
      versions = [
        {
          version = "1.0.0";
          hash = "1wglmmc71595y9aavywz1dj7hfw313acj2xn8pra088iw5hgs7fk";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "uzz5";
      versions = [
        {
          version = "1.0.0";
          hash = "0sg2dkyw301184ri6rxj08pjxs0i4wmpxkijy17yms5q7bgyk42w";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "v-1xbet-rfo4gksue";
      versions = [
        {
          version = "1.0.0";
          hash = "08kmpqmq20a200g84pydss1lg6myrlnl7g9d4cda0c99hh4ihx0p";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "v-antd-watermark";
      versions = [
        {
          version = "1.0.0";
          hash = "1h21xhjnh8i49lw2lnsali1jh5f6nxmklngdfk1iwliga9y1mswy";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wr-npm-test";
      versions = [
        {
          version = "1.0.0";
          hash = "120lv53glfvjzsdv8yi2gczn3qyin1qsnbif1mz2wcvl6p27h37p";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wraft";
      versions = [
        {
          version = "0.0.1";
          hash = "01fv0b7658b545dzbrq4gh037njb7hh2wv1cc4lxj8nn5nmhjhz8";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzx-cli";
      versions = [
        {
          version = "1.0.1";
          hash = "0syhagwrnwy5a54bhisapilfnaqlqd1kvgjpcmghp8hdawax96pc";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzx-event-emitter";
      versions = [
        {
          version = "0.0.1";
          hash = "0jpgy7k247xb9c2m4dz3328fsq1wlsw661g9yqdh1imd3n1amp6i";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzx-jc-ui-1";
      versions = [
        {
          version = "0.2.2";
          hash = "1j4x31jggkjdcx02229m8sjk7qxns6f5mwava3628b6032vz1k9s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzxwater-vue-library";
      versions = [
        {
          version = "1.0.0";
          hash = "1r5gf7x154x8x7kp5f0jzxzgbd3spb4c76n0cjv6aybh2rnkfr0s";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy";
      versions = [
        {
          version = "1.0.0";
          hash = "0fy94jfs7r2mrh1wssl9ggqxb8dhhlhhmklzsirhibfkycxi3scn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy-demo-wzy";
      versions = [
        {
          version = "1.0.0";
          hash = "19kzh1q13z77z1l7k2xk74zx1b8clys8g7bhb14m55wrf0pjzapn";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy-lh";
      versions = [
        {
          version = "1.0.1";
          hash = "03nzpvcqmvg76i8kckmlfxy3iyb0ki1b35d459jxi694hycq34va";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy-module";
      versions = [
        {
          version = "1.0.1";
          hash = "1y92sy09jlzj70afbwhjdnhb2qj21h7ywldvj2vxkfhqi76z0fj3";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy_moduledemo";
      versions = [
        {
          version = "1.0.1";
          hash = "04633l53l0vcj3a99j10zlyfyh57dbpw86gfxwj2yhzfhhs0n1km";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzy_utlis";
      versions = [
        {
          version = "0.0.1";
          hash = "1214w4h843igf14vs94ykhh515g2pr1ccwxc80y9s8bfj2sv8v0h";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzydemo";
      versions = [
        {
          version = "1.0.0";
          hash = "1wqvkqibsfq9bzf209gfhvf09s9pb1pblmjg22bvj6z4nqxw4p85";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzyexamdaytwo";
      versions = [
        {
          version = "1.0.0";
          hash = "1rdgr6jvi44c7xq18bwmx19cgx8kj7k15jqwz4q419z80mrx68ld";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzyn_toolguard";
      versions = [
        {
          version = "1.0.0";
          hash = "1w4110wq20g1hnv38dvsjci0mqbwbc1m5j5dijr797jg6b6yxgya";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzz-init";
      versions = [
        {
          version = "1.0.0";
          hash = "1pzzsqqswh2vmzbv2c3gdjp8490y7vd7w5m9ywxh7bv12cdbs08a";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzzcli";
      versions = [
        {
          version = "1.0.0";
          hash = "162ikzs6f27hvhlyc8kcg2gprawajpllpl76p384s617vm507a4k";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzzd";
      versions = [
        {
          version = "1.0.0";
          hash = "0n079crj13z0zz6zqw86zdpd0iyjn9g7wi8pdbpxy7xisbyrfssg";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzzy11";
      versions = [
        {
          version = "1.0.0";
          hash = "0ppch5yxs87bfg8ifnjhqiw4vnan5jbx7w88jv8vsh37c5qk8b3n";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "wzzy2";
      versions = [
        {
          version = "1.0.0";
          hash = "1nqhwdjxrn8kma7dh3b98bll1w39i4z952cz5xi1y2hsyd6v4jig";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yqzh-test-lib";
      versions = [
        {
          version = "1.0.0";
          hash = "12ngcqwngdp6wb389dl11qf52qg6a3bsa5g7f8gi52mxk4c057ir";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yr-forecast";
      versions = [
        {
          version = "1.0.3";
          hash = "09rcbcy873cwa6x7mrh6624f4pcgykq06xj0xr4b8jzckaa989kf";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yr-logger";
      versions = [
        {
          version = "1.0.2";
          hash = "1hk68d6i964fy0x8h0zwlg0qi46k3rwkmq5sd7mdsy3zzqlmkc5h";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yr-random";
      versions = [
        {
          version = "1.0.0";
          hash = "1621hfirvhprchybjccz1lx1b33p98gg1dzy83l2j8wwrni6rk9j";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yr_first_pac";
      versions = [
        {
          version = "1.0.1";
          hash = "0zf4xzhj84gpbkgb9rwkzixpzmp6ldbhs8knm19l4ssbxfj8jh3v";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yr_math";
      versions = [
        {
          version = "1.0.0";
          hash = "0z17171dsi72y92rvabrivgnqhrymzr9f0r0za6vx5mqnywpjnhh";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzx-npm-demo";
      versions = [
        {
          version = "1.0.1";
          hash = "0yxraaxs512db49ph0f0bmm3i44rni31jhnxmlikx84grgwxnahc";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzx-qxazusa-xyz";
      versions = [
        {
          version = "1.0.0";
          hash = "1a4bqcl10423xjlmzqhya5dy5vc9mq9cang13p3wyxfq1mv34ia5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzy-mypro";
      versions = [
        {
          version = "1.0.5";
          hash = "1zixk9pb72y70hrq8ah7rp2whphzccxyfrmzawqxkh7prkm9vrqh";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzy-npm-practice";
      versions = [
        {
          version = "1.0.0";
          hash = "1nmbwv5nrc5p5bbn2glixfa53660dh64s2qb5fdlnp68s5mf9wan";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzy-yzy-yzy";
      versions = [
        {
          version = "1.0.0";
          hash = "1bar85rg2kj42c0awqypjs373p4rq8xaclpf2xr73qswrjcrz631";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzyfastcp";
      versions = [
        {
          version = "1.0.0";
          hash = "0p88m4cbw7qhgz1alcfwf76lry0jiiyijbhrw8v53937ayb694v5";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzz-cli-test";
      versions = [
        {
          version = "1.0.1";
          hash = "10n3npy8378rsf7yk4701lqajh7px2rc1m92hzigz30lz2v4z3xx";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzz-tools";
      versions = [
        {
          version = "1.0.0";
          hash = "0rb59i4196i5rn41z8cnyyzzlcwylkpy5npf3w0pb32cjybrgqh1";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzz-webpack-test-plugin";
      versions = [
        {
          version = "1.0.1";
          hash = "1j88x6r0ngqiv8janxd9243zfi1jink3sfrhi16nwxdsnk81kly7";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "yzzcalc";
      versions = [
        {
          version = "1.0.2";
          hash = "06430g769rhif0ngb55ds669wnrdm2vibd53qch2ncqwbkrmf51x";
        }
      ];
    }
    {
      ecosystem = "npm";
      name = "z-accounts";
      versions = [
        {
          version = "1.0.0";
          hash = "1fhqn03msrqklmj33bk20xg2fdssr1451zmb7xm6arbfxhp2d448";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "adworld-render-worker";
      versions = [
        {
          version = "0.1.0";
          hash = "0zqbc9sqns74hnz5qvkn2dzlfikcy6vbm649x663322vw5jjvbh7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "aiomysql-core";
      versions = [
        {
          version = "0.0.9";
          hash = "07pkk26xx4lyzs3nsb44al92fd02g7f7xqb3gk07vi37wh1sd4w6";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "alice-demo";
      versions = [
        {
          version = "1.0";
          hash = "0rdfk7ij9z5hljrrdg94xg3cbs84017r627xwcfy3yvs17wxpj60";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "am91-gaia";
      versions = [
        {
          version = "0.0.3";
          hash = "00m0xcg57p8hqjq5cszvfhvhpi7przrgmcf71i2h9imdn4nxglxw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "arithmetic-ouster";
      versions = [
        {
          version = "0.0.1";
          hash = "0xz3d3zw55b1pk680hnc1s918fvl9jnws8i79ngirccpdhnnyy6r";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "azure-blob-operations";
      versions = [
        {
          version = "0.1.2";
          hash = "0lyw6crwzarn23zk6zhbzm9lgal12i70bp5ynym5mgxim49v11hp";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "barr4crypt";
      versions = [
        {
          version = "0.2.2";
          hash = "0pp0lnf9gv20z1vsb91kyfawr8fydrb5dmh4zzdj20fg2flwwwb7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "bc-analyzer";
      versions = [
        {
          version = "0.2.3";
          hash = "1gifqrqddqyxcw7c2iav39pg2y12sp5f1if8sg4c7nplxwvy9wgy";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "beerus-test-package";
      versions = [
        {
          version = "0.0.6";
          hash = "0z1xv97aaj7fdakzy4c6pkpxmcr0i97nv95fydczqg6j6vnxz7ns";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "c42482de-725d-45a8-b9fa-9394c513fe12";
      versions = [
        {
          version = "0.0.1";
          hash = "0903h00m2zdhvyx23xfa0qgvyp43hbx4hy8x7vgrvl3pq56xq8g8";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "carpi-redisdatabus";
      versions = [
        {
          version = "0.2.3";
          hash = "0q2j754n1nrx1rq05c9hi3an8w3a0ln9n4dpck3gc94219f1iixi";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "cga2121";
      versions = [
        {
          version = "3.3";
          hash = "1qqhz3w9fkx96scb18v1iwh6cdvx8kdv7x1qpzkhzcrkrl3x8bln";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "chait-test-script";
      versions = [
        {
          version = "0.2";
          hash = "0lxcmgzzpia326cwgfynsh45vscvrjd3cq2pxsg6xh9afaw0pvr8";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "class-argparse";
      versions = [
        {
          version = "0.1.3";
          hash = "0vf7c041mkzk3dlgf7kmajvrg8pa1bmg8301krg7y5v1x7v465n5";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "clyngor-with-clingo";
      versions = [
        {
          version = "5.3.post0";
          hash = "1ls1rnn3w1abw0w75xy3qffpsip9gz27l3lv26jxabr3rc8mi43w";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "colab-dev-tools";
      versions = [
        {
          version = "0.0.10";
          hash = "0kr0kk7q7bc6qhxwdn30l4ryh5byga3i1gl9cgz91zm09cvdv9nz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "comapsmarthome-postgres-client";
      versions = [
        {
          version = "0.1.3";
          hash = "00n3vwkp1nn531fi4rph0779apjxw8i90bg5grzj1j7m40qfy6yr";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "deeplabcut-pytorch";
      versions = [
        {
          version = "0.0b0";
          hash = "1wvwk5s1fyzgn73hyfyy9kj5aqkd5r1gm7m6ppd3bky78blhf1yg";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "deployme-installer";
      versions = [
        {
          version = "0.1dev";
          hash = "0l67bp8jjk6rn000d3ddc1yh8z4hy3if22azvd1lqz78ffhy6cxi";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dicto-pkg";
      versions = [
        {
          version = "0.0.2";
          hash = "146n6w4xailnscwyb9r2vnd8km4s2m10agb9v2gx997j89pvfv9n";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "digit-index";
      versions = [
        {
          version = "1.0";
          hash = "1lbng5w0lx1ffpy84rcaz7zg6nv0jvx7bk1s1ffw12plgwpcw7k3";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "distributions-example-udacity";
      versions = [
        {
          version = "0.2";
          hash = "02ypr1g22l6fpbfjfgi5fxfi21lv9b4m868zdsh2hsxlmnf7rwyh";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dj-annotatable-field";
      versions = [
        {
          version = "0.1.0";
          hash = "023g7xx4n2c9xildy1zw3sy6bxsdsjhv35czki3i05vwcrga31ii";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dj3nk";
      versions = [
        {
          version = "0.3.0";
          hash = "1g5yqz2q97p6sgdwszbvbz1d4wm1p5ism23dd4xqfvpyl62j0wp2";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-aws-secrets-env-setup";
      versions = [
        {
          version = "0.3";
          hash = "1lrnyszdvhmz3lgb8jccynh3j00w42whmzppzi3gcy594b3g2xn7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-board";
      versions = [
        {
          version = "0.2.1";
          hash = "1rpl3382m20vg4dfsa5b4acd165a5g83bakvb4ipq9xfjpn0ybg2";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-cepfacil";
      versions = [
        {
          version = "0.0.1";
          hash = "03rww2famg7rv9w38ndvhax9rqih551300zmjivs6m7b2f3fkvps";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-cloud-essentials";
      versions = [
        {
          version = "0.0.2";
          hash = "0w0p8cnqfs08kywrbjcvpdhvzx0fn56n7pwhyv49jvgjyns2npns";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-delivery-areas";
      versions = [
        {
          version = "0.0.1";
          hash = "0b9awc0f6c5arxs330zm3gr6s2qgpfhmsvi6pg9420vi5jlx8b0j";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-feedburner";
      versions = [
        {
          version = "0.9.0";
          hash = "0lhi9l4iyxrxs873brhgdyph5jpxsgz826vvm75f5ww2kfiy4gqh";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-field-translate";
      versions = [
        {
          version = "0.1.3";
          hash = "1wxjvm9vz29r89aafwn0pxbxgcsapbwkw01w8biymhcwsijig2bc";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-pgtree";
      versions = [
        {
          version = "0.1.4";
          hash = "1ll9vs0qhi11r1flxsm0idmb3xs2j94jk1nsbrgwfgh531c6v05y";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-rax";
      versions = [
        {
          version = "0.6";
          hash = "0m2hs15bsc8b6hsksjfz97yz3ywfr6f6s7kl2bsbb7kals5mda3j";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-rss-widget";
      versions = [
        {
          version = "0.5";
          hash = "1zb60g3fnpfczcb21iw5fhy7dh2m249vgqgddghjgii6l5qa468g";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-section";
      versions = [
        {
          version = "0.0.3";
          hash = "1rdc6xckj5gpdssq00p75vq0k6yn6xmxnp69wr37y1yrfy41q4l8";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-singleton-admin";
      versions = [
        {
          version = "0.0.4";
          hash = "00q1m0jm685andg3iw015ryra4ds0xzx6v5k3dp1jpza5z1l57d4";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-standbydb-router";
      versions = [
        {
          version = "0.2";
          hash = "1dic1nmi38vph7wgif1b1qn3zikccvlp2wvmll0qfclzi8v2bm3p";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "django-validate-on-save";
      versions = [
        {
          version = "1.1.3";
          hash = "0lrdbs5gh1f8d0whb2b681b6a6bk69lmfvc1fils1m78si42i1m6";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dms-to-decimal";
      versions = [
        {
          version = "0.0.2";
          hash = "1ynlzwh31g2vv27ni91z38kpfc9ixz0m1mfh91waq1s6kz7xd5wp";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "dogebuild-fpc";
      versions = [
        {
          version = "0.1.0.dev1";
          hash = "08zgggz3a6sb2v9vjbis18cda94mi6x14f5s7iy0ddh041nv6vnh";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "drf-model-serializer";
      versions = [
        {
          version = "0.0.2";
          hash = "05hgw18g3rhpdh01gwnnkpz8a9i6l1pdny048ym26r5chw9x466v";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ds-logo-detection";
      versions = [
        {
          version = "0.0.1";
          hash = "1n9bfabids00jnfy4aj2ybjmg52j2af98nj54g7yq2h5cs9kalmn";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "env-flag";
      versions = [
        {
          version = "2.1.0";
          hash = "011gzd9zc9qhgfkipfnqdxzcmwi79c8bichba1ij6w9g1fqscs85";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "eo-learn-geometry";
      versions = [
        {
          version = "1.5.0";
          hash = "0dlxmi74pn8cmy19z41hd1cc58mzrhf3ps3b0shdm3383py9zcp1";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "eon4dice";
      versions = [
        {
          version = "1.1.1";
          hash = "06zmkickhy3mx3zvww7z1n9z8acmgxsfq2s21pjl1gi40rcpwhgk";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "epy00";
      versions = [
        {
          version = "0.0.1";
          hash = "1q5v80yk9aw26idcb8akcz0jig1rcn6n7yfd0v7zf9ifyrva2qb7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "example-pkg-kingkong";
      versions = [
        {
          version = "0.0.1";
          hash = "0xqqib6ms5m2hixfqfqbf470frxln919b6551c2r4b9dvb9ab9j1";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "face-group";
      versions = [
        {
          version = "0.1.11";
          hash = "076kh0axnwr1fl48vlqasn3cvi44lfa8y59ryiphr005f8pl0l0b";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "failrunner-django";
      versions = [
        {
          version = "1.0.4";
          hash = "0sylahl73l39j96zx3xslffj0azqzw26f1yrqhfkfq4npfcnynrs";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "febraban2";
      versions = [
        {
          version = "0.1.1";
          hash = "1y5fs9g5xynqmh8f4rbrwarbzrswl9nfccn7fw9vivbfj5n1rgkz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "fire-cli-helper";
      versions = [
        {
          version = "0.2";
          hash = "0iy41ay64z5iyi27931ckcfw9a26aq0ral8ya0f100vms88xhk7j";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "fl-static";
      versions = [
        {
          version = "0.0.2";
          hash = "1hpg0kzfa8ij1lgjhki0yxs7wz7d457j9kxmppwhq63mq60xlpjz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "flask-ipblock";
      versions = [
        {
          version = "0.3";
          hash = "0fg9dcv5qjs9acdp26rp73v4l8bffcv943dnprlss0anvapg2ski";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "forgot-again";
      versions = [
        {
          version = "0.0.3";
          hash = "1qics1h0h17mpg6rql2zq5gn1j7iy5swpzm5jlm4ahjy2x1f5c1z";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "fp-growth";
      versions = [
        {
          version = "0.1.3";
          hash = "1x25156wl7km5h3w5wnsy8i4mvlmb94ma47s5g57hj252gkhagmw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "frasco-api";
      versions = [
        {
          version = "0.6.1";
          hash = "1c9mrbj0v6sa3gry3l73y1zvjv2vsrmzc28qrhalzl5gi3ra8qlh";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "fwmp3";
      versions = [
        {
          version = "1.0.1";
          hash = "1dmnzmndhrpnsbiaa8rm8gn1rildplw1ipfp18mlf39gz0c6g3bv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "fzj-hfcam";
      versions = [
        {
          version = "0.1";
          hash = "1wlrc8jdg98ayrylq0sa2qkgldqg4v115jp70jkzqz87cipz3bgp";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "gym-2048";
      versions = [
        {
          version = "0.2.6";
          hash = "0qm61w527vkz7rnzvh9z0vmqms2am6zn2s5b7vrnsmgfsl65vc8k";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "happy-little-helpers";
      versions = [
        {
          version = "0.0.3";
          hash = "0dkysz7pcn6lrp2rmkjl42frqpy5758517cmgxwxd5l9312fn3qn";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "hs23005uno";
      versions = [
        {
          version = "0.0.2";
          hash = "08zmivqvl16i75kd45gqcbxx336ifd79cj0gx8bc8gqfls8m0wn4";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "hypothesis-django";
      versions = [
        {
          version = "2.0.0";
          hash = "1j6mq79ab4a8xrh07p5cas2vd8vajr5z1nhdsnz894ag7sxm3k65";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "image-inpainting";
      versions = [
        {
          version = "0.0.1";
          hash = "14lm5j44i86y08sy60af4z0d4c4nzbc33ws6xf5rwgjg52p3c4l1";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ip138";
      versions = [
        {
          version = "0.0.1";
          hash = "1j94cqkyqgzmf4lx17in4hmqbhydayz4wj426jsspmfmykcff4i5";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ipaymu-python-api";
      versions = [
        {
          version = "0.0.1";
          hash = "0p4jhyzyhs8av2ha6wkzpl3kdn9013943606gz8dyh3z45iqfadl";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ipy-show";
      versions = [
        {
          version = "0.1";
          hash = "0mrrcw1033040i30dbvr2q0qqk5y3kdqna0vs86kq6yzzvibkfkb";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "jensen-shannon-centroid";
      versions = [
        {
          version = "1.0";
          hash = "1fq6gnwl7qgczz5c37f3i4h5718d8frb96kq5mppxj3r63ygsg94";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "kafka-lib-tima";
      versions = [
        {
          version = "0.5";
          hash = "1gihlan8kc6y822y9hqd6ak1xsyb2fas77b6hgc7m6b65dgi79n9";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ktuvit-api";
      versions = [
        {
          version = "0.1.1";
          hash = "0j2cvlyacvg38ndp9gwfkjadbs52lmc5r31wjj0xhjdwc1830k55";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "labels-local";
      versions = [
        {
          version = "0.0.1";
          hash = "1qgq061l285qs43a4cds5j44jsmp37087dfg2mx3rxa5859fr94z";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "leonardo-import-export";
      versions = [
        {
          version = "2016.10.4";
          hash = "0q0khjy05cl12v0l6avbbcyp67r9nb0lm5hjq76m8afjadlm9nf0";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "levy-stable-pytorch";
      versions = [
        {
          version = "0.0.0.3";
          hash = "05m6ycnkkz4aidiqcw5cnb5b6jny393n3d7bfdm5i3dvkhky9ik5";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "libre-crawler";
      versions = [
        {
          version = "0.1";
          hash = "1hmrrd4qp75byisfz322r7zl5brij7dlxcqcv3d2f4fccizj7cqf";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "log-util-huynhnt";
      versions = [
        {
          version = "1.1.0";
          hash = "00mfy0jxqh8nlgsapmxlzdp1z5pscpssyj1q8ibilkmd2hkjnz23";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "m4s2mp3";
      versions = [
        {
          version = "0.1.3";
          hash = "1y9w1hld8vs341bkbnlvh4yfa6fdndv9v5cz2qwlrg9vrjsrkgqk";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "map4-cli";
      versions = [
        {
          version = "0.0.11";
          hash = "05bhrfdf7wn0bs8i0cvqkycwg84vxahdzxsg96nmjiks15gxnx9a";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mapp-airflow-extensions";
      versions = [
        {
          version = "0.2";
          hash = "1ilccs7yw7mx5xk97q02w320pdkh6194nmj16ij26ihg18pzwpqd";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "material-parser";
      versions = [
        {
          version = "1.2";
          hash = "1lkxcys7wvvr6ym828v6xrmrya5zxaphqpffhbg60p37a3kjsp82";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mec-translator";
      versions = [
        {
          version = "0.0.4";
          hash = "135blyz33wlhki4f3c3ihaidywb4v3ldifavpla377qaln8qqsln";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mkdir-p";
      versions = [
        {
          version = "0.1.1";
          hash = "17gy4q35byn61csr9fsd8awiicbvan8gl0j7ca8r9jgri5f67r3g";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mkdocs-gitlab-review-plugin";
      versions = [
        {
          version = "1.0.2";
          hash = "15r0lq6lvm32fsh0rh7c33p5adaz8k6rpvp7ijc4kx7wds0drfmz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mozart-signal-parser";
      versions = [
        {
          version = "0.1.1";
          hash = "1ra9xmd37yd71lwvc6rbkl1svxfpzpfh08w2kqlqgw0976d4isdv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mp42uni";
      versions = [
        {
          version = "1.0.5";
          hash = "1yl73x13ssvhz4pzq9grfqigqdfjrjz2p9xsbn7if91z0j8fxlck";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "mra-tools";
      versions = [
        {
          version = "0.0.1";
          hash = "0vgyp3ksp5f1r6dqd9mpcb0n1d3gvpsjw6zjjn0xj1yxvqkb5ybq";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "multi-folder-pkg-deps-insomniapx";
      versions = [
        {
          version = "0.0.1";
          hash = "08rcdqmvj38qnbsl1b9l3r28hxjljm046f0v1a8a91qch9cv776k";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "multiply-test";
      versions = [
        {
          version = "0.0.3";
          hash = "1ny562g2xqc04wsx8pm22b4x905v261d6mk8srklpgrsqw3fi4sw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "named-entity-recognition";
      versions = [
        {
          version = "11.0";
          hash = "1l6d2zzid2jr92vya4xi2dks6bxlc9za3y3m6dv9l4lizpjvzxnd";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ndx-labmetadata-abf";
      versions = [
        {
          version = "0.1.1";
          hash = "1pj0ap7q28da3iskjwcai0j5qy7d1h3nv36sfh9q2rvnggnjfmp8";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "nester-kk87";
      versions = [
        {
          version = "1.0.0";
          hash = "02cs8sbxk7clnj4ns5aw6r753hs87h6cxz21nkb8jw7mf5sbkw09";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "nvidia-cufft-cu116";
      versions = [
        {
          version = "0.0.1.dev5";
          hash = "146n7xq7ji54gpgrswidz2wlzlrarl7xd7fkybbmf4ymqk12lk1p";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "nvidia-nvtx-cu115";
      versions = [
        {
          version = "0.0.1.dev5";
          hash = "0cdayg31wd4d0c5rgybqm9fxwhgpgra2z7k6nsg7ami8j2yjn2gz";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ocr-varianrpm";
      versions = [
        {
          version = "0.0.1";
          hash = "0sl4n9zz80fw0i9j36jsmfn542gp7qq77gp80j4l5w60c3wk9d7d";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "odoo-addon-somalimentacio";
      versions = [
        {
          version = "16.0.1.0.0.2";
          hash = "1bc4qab7khr7sbpixpwkgr4r1mhdfld9gvgi5469hrcgdh3cnihv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "onlinecheckwriter-quickpay";
      versions = [
        {
          version = "1.5";
          hash = "08r6ph4va6wy5hlm7zs1x1kj5s0gmk1zaz9h6ksdswnl13s3n9jq";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "opal-referral";
      versions = [
        {
          version = "0.2.1";
          hash = "142ls2dg2r2im0j7yiffl8qp2z504yvk8ghjra422s03gfjpnlns";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "opsys-relay-controller";
      versions = [
        {
          version = "0.0.3";
          hash = "1hd5g8895c0hxqwaxh4js351kxc393amgyy1w2nvg6ywcp34v4yy";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "packageuser20";
      versions = [
        {
          version = "0.1.5";
          hash = "00pcij904cmfh2wfaar1jcp2k0rp2ms29hl9is5l8d86qfhda644";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pacote-de-processamento-de-imagens";
      versions = [
        {
          version = "0.0.1";
          hash = "0jv19klw0fs50rbfrxwaxg0ly5fwnrzd2d1rdvn0hs6cq30ry178";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pandas-optimum";
      versions = [
        {
          version = "0.0.6";
          hash = "0lapxbr9wgxz7bf7mawv23mc266figxg7b3f84i7kva5l9dkcbd6";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "parallel-runner";
      versions = [
        {
          version = "0.1.2";
          hash = "1jarnwlhi9y03qr7cqjjg1axildjlz79iq8qx6yh4p6j8idwzj1l";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pelican-bootstrap-figures";
      versions = [
        {
          version = "1.0.1";
          hash = "16z4v5lzi0zy7s712x3z5zpnlfh6m7sp3hrl7bbkz39gq5d21d1z";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "presentation-mode";
      versions = [
        {
          version = "0.1.2";
          hash = "19m356j388pigical3l6qki1zyk0j6qj3am2pcb00hjpbndvi99b";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "principal-fft";
      versions = [
        {
          version = "1.0.5";
          hash = "13djdpaxsl48jrh8is4ax58gpaz9zv03x68ka1100i44536w5rh8";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "proteinbert-pytorch-reproduction";
      versions = [
        {
          version = "0.0.2";
          hash = "1k6q514sp47542nwrrwwdmxy0lz2kzi485bvj0r72i1g72j9yk6z";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pubkey2address";
      versions = [
        {
          version = "1.0.0";
          hash = "1flin3j0k0jgsj3fasw3a1mdn0j02y17dqnz6ir60pi9yjnvnvnv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "py-hiit";
      versions = [
        {
          version = "0.0.1";
          hash = "14cn1ma30hlb2a7vzb0326mginnbv2pqq4r7wvili5lxb4s1sv07";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "py-retry";
      versions = [
        {
          version = "0.0.5";
          hash = "0cc833si5sbgihq2i85czdmbq3kdz7i3clbil3x2j8nbj36sp2kr";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "py-tgb";
      versions = [
        {
          version = "0.1";
          hash = "05aaqgxcbmnqx5b6mp9lw2fkqaa9cl4azg3f2ylai57n9xpsj8mv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pygame-crt";
      versions = [
        {
          version = "1.0.1";
          hash = "108w91wynyjmn3klapx5qljyz1nhhg4mphi145fipnjpfin7sndd";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pyspark-event-correlation";
      versions = [
        {
          version = "1.0.2";
          hash = "0lp13rkvk9fw3snxrjs0g6y2bad3ff8xz8xkpgp1bh8swglhgr5x";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pytest-auto-parametrize";
      versions = [
        {
          version = "0.1.0";
          hash = "0mnf9gj5098nwz3pby3brpxkc95cnp038xwp72ap93f543vxk1si";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pytest-django-cache-xdist";
      versions = [
        {
          version = "0.1.1";
          hash = "1v6li4qwbmvpg6q4hkq8xb67qdkpch74avxj1ccbx8jqv4942k79";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "pytest-memprof";
      versions = [
        {
          version = "0.2.0";
          hash = "0rxg0q3bwlc9iax0skpia3a5x7xijz9i5110qk50vf9avwg9zg69";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "python-fluent-log-formatter";
      versions = [
        {
          version = "1.0.0";
          hash = "1kla3xyhcjiby048w3pg7lcxvrdkcl4ynxw5vxbnac2jff25zr28";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "python-parsekit";
      versions = [
        {
          version = "1.2.2";
          hash = "1sbm9sr9p91mdsf9cxyqmz91qm4k8g63gixx937f5wda4bjmmy7n";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "python-qbo";
      versions = [
        {
          version = "0.3.0";
          hash = "18daivpmkj6sg39d2p0lcxixlgy9cw1pgkwfkxdx3x00rnin9prd";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "python-sochain-api";
      versions = [
        {
          version = "0.0.1";
          hash = "119bfz1z3xq8ij5v8rv7k1dc9jjfllc9wm51nmy5wkxyq6fbyl5g";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ql-sdk";
      versions = [
        {
          version = "0.0.4";
          hash = "1yq9kf4s8pkv72mxyzxapq7v2kjgk2fkxd9ij5m7k1sf4h5hzqf2";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "quotes-wrapper";
      versions = [
        {
          version = "0.0.5";
          hash = "0h2pabmqizhnlcxnz237gjvhgk3q5qbgdf5wi7f1iyifh9krj0df";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "route-visualization";
      versions = [
        {
          version = "0.0.1";
          hash = "0vhlyp2aww754a21wnav4y3ahpl6ncblq88baagx86gd0fh810g9";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "scrapy-script";
      versions = [
        {
          version = "1.0.0";
          hash = "192746kiwg7p5nq7bkvcxbrqn60w26bj5hn0n4k5i2sr0ikp70ik";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "seamoney-perfcheese";
      versions = [
        {
          version = "0.0.0";
          hash = "1x8i037rjd95iay4c6hrj3pfs9z245qwjb3rg91lyn7gxk5jmz6p";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "sentry-kafka";
      versions = [
        {
          version = "1.1";
          hash = "1ac0f39rh833j1pb28v2vygpnrn9vmca70bqvwn474qany0kkiwl";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "simple-engine-core";
      versions = [
        {
          version = "2.4";
          hash = "0pf0izinsfm4l93qbafny0wfy6fkxhx39pn5vnhzl5iyfvdk8003";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "sla-calculator";
      versions = [
        {
          version = "1.0.0";
          hash = "0vc0fwfyn31g6xdcd540ypl6idc9mg2awamcv16cgsk1zn6dc0gv";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "slg-test-7";
      versions = [
        {
          version = "1.0.3";
          hash = "17qska8664svn24bx3a2z3q925jxcxb28z29ayqk92p1yw0cfawm";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "smartthings-rest";
      versions = [
        {
          version = "0.1.2";
          hash = "1gzq5dyrfklrxwh8vblzw8llnzvmmzy3gw7pyxpvy16yd7rikar0";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "sn74hcs137";
      versions = [
        {
          version = "0.0.0.dev3";
          hash = "0hkkqh5q7gbwn1d87rm1fqpha62zjx44pnsfdmdjwwzf84pbmjl3";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "sqlmodel-serializers";
      versions = [
        {
          version = "0.0.2";
          hash = "05xddpfahi8prhqlsa3csnzh9gadpjjjml9gyjihwdjyc56xd7k9";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "stati-redis";
      versions = [
        {
          version = "0.0.7";
          hash = "1bwmnhq62lk56ripipi69xmh2jmm70damddp6aca0rz7v7wr40kw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "streamlit-flow";
      versions = [
        {
          version = "0.1.0";
          hash = "05c9fz64246qkkcwjizsfdbiar070r9167zkwx3nlzvsjsj78qbq";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "tapioca-meetup-client";
      versions = [
        {
          version = "0.3";
          hash = "047k534ixpaf6yrsd8cki2gszzg0g8h8y91sx427513raa573q22";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "tempconverter2025";
      versions = [
        {
          version = "0.3";
          hash = "0i39pi2mq7vy829963p4yff1hprg1ifj72rsxszy1c43n7nlasp9";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "to24hrs";
      versions = [
        {
          version = "0.0.1";
          hash = "0lczrqzh5dzdrsyimb6s5ldbskffvr5vjgq007vz2iy3wlwgslp9";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "toposis-parth-101853019";
      versions = [
        {
          version = "1.2.1";
          hash = "0klwgsiizzmk93n97r6yj0k7bxd2lh79mc75byhqnc07z1n4pvfb";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "topsispackage3280";
      versions = [
        {
          version = "0.3";
          hash = "0wn7wygnwj9c9w3hcwj6k7j2ijg2srrwpl5najlrhg5p3xq3xjs7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "tornado-requests";
      versions = [
        {
          version = "0.1.2";
          hash = "1y087fr95wva0rzp7cgc4fc629bayh2c52a68l68fch3annkxq8k";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "tsuru-router-tailer";
      versions = [
        {
          version = "0.4";
          hash = "18b8np0j5kjvjl29ff5ki6cx6fykjsw0gb17d3g77m7scmkxj465";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ubit-autolab-auto-submit";
      versions = [
        {
          version = "0.0.3";
          hash = "0sl5z16vfjv0dw2p0mdgm5wnz5cds9bwa5lv2i475y1xal0rak7r";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "ubit-autolab-commit-parser";
      versions = [
        {
          version = "0.0.4";
          hash = "196dpj9c6k052hhf4wrlzrlxhjkxrw2jgxg4lnkq1i71lrqr79c7";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "um22006uno";
      versions = [
        {
          version = "0.1";
          hash = "07nknivsbx5qk9zrwkv1689jm3xdx4krxap16kfp649cmpj8k54j";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "unicode2hex";
      versions = [
        {
          version = "0.0.1";
          hash = "0lc7rsjmlvgwqj84gjywi1jv9bj9hnfzki2wd82806l0a1cn2psw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "universal-crc";
      versions = [
        {
          version = "0.8";
          hash = "1nsxnvs4x4iqh5dv4gkjcjnhk0b3dml51bnwja63b8cz2ygqq57v";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "wix-protos-devcenter-app-service-assets-api";
      versions = [
        {
          version = "0.0.1";
          hash = "1d51gz5klv428wb0f816vcp564lkrjbhbyksnzg317pnp3ds1irw";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "wix-protos-proto-site-assets-module-lifecycle-api";
      versions = [
        {
          version = "0.0.1";
          hash = "1jp17sn52ds8qnrxq3bh2b6wykmnca8w5j27hbrrnawhjjdmpspl";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "wix-protos-test-logging-logging";
      versions = [
        {
          version = "0.0.1";
          hash = "0pnb3b2714lq5p733a9ya3g35wavbpqp20qvi9swr7knylhcigrb";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "wix-protos-vi-snapshotter-backend";
      versions = [
        {
          version = "0.0.1";
          hash = "09iriw6h3c6mlgc30r2q8d7z3lka5g74mnp4l1xzkbgrb7y9lnbj";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "xitroo-api";
      versions = [
        {
          version = "0.0.1";
          hash = "0cq0fq1jd2gm8v0qkj9rkjvscsisb7dz1y1rdh6kd7daghva5kq3";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "yplan-logging-utils";
      versions = [
        {
          version = "1.0.1";
          hash = "1klm12r1ic4d3aryc0vgj5h2mv7p8151mwbrc7ig8gvxgaqjxs5q";
        }
      ];
    }
    {
      ecosystem = "pypi";
      name = "zimran-django";
      versions = [
        {
          version = "0.0.2";
          hash = "1rizqqfr2a2ni0ggwva3dw3k3mh2mdamc4fpjisl6i9wzrkqk236";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x-posters-planers/botforeverythingandnothing";
      versions = [
        {
          version = "v0.0.0-20240320105940-58eb887197fc";
          hash = "1z839c4k0v7v3vyiryk2pp7a6i5gxpwbky7977158qdi8j23z13i";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x-posters-planers/botforeverythingandnothing-uninstall";
      versions = [
        {
          version = "v0.0.0-20240320105942-98faa2b30fa5";
          hash = "1sgpz8xdpcz54q07sr3v0g3jb27vqbrfyb5zwy47kz2lv2sfz7jv";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x00-pl/update_deep";
      versions = [
        {
          version = "v0.0.0-20170628144025-2ca7693af353";
          hash = "1x6m7rdhfyajl85k16anr9qr5ighkjz352xymf3jhwdbsi5xk429";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x01/beddit-api";
      versions = [
        {
          version = "v0.0.0-20141007170746-627612c0ed07";
          hash = "14drxl8d6cxlf3dfryyi8p41cki6g68yrdisdy4r2splw05vk5ii";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x01/gulp-map";
      versions = [
        {
          version = "v0.0.0-20160324130954-93f123a9626f";
          hash = "0yypbmyzgbyjvjqfizrxdin90i88lzy4gvp9l2f82svvs9c83rhi";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x0a0d/asin-amazon";
      versions = [
        {
          version = "v0.0.0-20200308140859-4fb86a63751e";
          hash = "0x7vqsi357sgh30ap0pfxi8r6xrpf6v63ax6zd753c56rab721bg";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x0a0d/firefox-latest-version";
      versions = [
        {
          version = "v0.0.0-20200820045353-c34ebcc53fb0";
          hash = "05fxdd1gmvaw849c9ck72rrnvizj3rplggcd6jpqb8j9lziz16gj";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x0o0/damn-sol";
      versions = [
        {
          version = "v0.0.0-20240331162326-ba01ca94f9a2";
          hash = "0dhfy53rn2ml9fzgcni3fi2xf9qmx0065vfz0ikay4v95wvz4dn9";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x4139/bashbin";
      versions = [
        {
          version = "v0.0.0-20140929183619-dd6192000034";
          hash = "118jg0yyqflficcmjg9ybrwyi5d8agnwbl7igmwfw2898mfy0yzi";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x4139/cnet";
      versions = [
        {
          version = "v0.0.0-20150811131605-09eee81235f3";
          hash = "1gfshl1gvx9mk1rqlxa45xkg0bjwyb6fvcgk5d8jz1n91xplhv0d";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x4bd0/randomstringsgenerator";
      versions = [
        {
          version = "v0.0.0-20200115125804-51e6d5dfd5f0";
          hash = "0ld99qwgkc3vs6jf4073sx8vdrx7hfl6klyzzdrrzc8y2rbiq4f5";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x7f/readline-json";
      versions = [
        {
          version = "v0.0.4";
          hash = "1l080cd4cviwlijpd3kx3nl45ww2wm7g8v4nw96vqwpdj12gjqfk";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x80/firestore-facade";
      versions = [
        {
          version = "v0.0.1-0";
          hash = "16a0jbhcd39nnpqh9k6wb03lh5z9b88l0kz81nz61hqj30q8x309";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0x8890/eslint-config-0x8890";
      versions = [
        {
          version = "v0.0.0-20170621151027-127345bbb1dc";
          hash = "0kd0r9ba4vf1zap3ssjrqp674p0y7cdmnlllh4yz7j1gs403x78x";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xa02/devi-indah";
      versions = [
        {
          version = "v0.0.0-20240328163342-a10788a17950";
          hash = "054ydvacdyknip6fw4spnhkphgqfhzly2bk7daprc6gqylq8fkqi";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xa02/imcoming";
      versions = [
        {
          version = "v0.0.0-20240326153051-5d21d4a5f9ba";
          hash = "00j0ch84p0mivn9dl4xzz9rgbs0gmc4s15a31mb70hyqarz2m1jx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xamogh/dont-export-your-pk";
      versions = [
        {
          version = "v0.0.0-20220913152731-a293586d2671";
          hash = "1vndyrrmmx4an5rw7f4yvlm5rcr92v5h5w2dp6nxsaszxd1spnqp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xandybarlow/oauth-parameters-registry";
      versions = [
        {
          version = "v0.0.0-20241015191331-3c645981ea03";
          hash = "1r4q3k53bfxz3yz6rrb0sq8w1mk3n8nf2zs6njj997ccyzx5v1y6";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xbeltalowda/monocloud-breakboard";
      versions = [
        {
          version = "v0.0.0-20240320031513-6d61e2729ec4";
          hash = "1lgqjwxw5w8vfzrrgm5npdx0a8k2axyy4nh5zxhfxlg5rd7nxxn5";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xbeltalowda/monocloud-dashboard-fetch";
      versions = [
        {
          version = "v0.0.0-20240320031506-409e45da9efb";
          hash = "1dwip92hsw5spkjlr67z8mvn6as5ka156l30dily783gp2jgly39";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xchristopher/vue-dynamic-charts";
      versions = [
        {
          version = "v0.0.0-20240320114028-0ec51e86c0d2";
          hash = "0r4rz867x30d2h19c1grjrx2bqldc5vlw66vivxi2n9bqysbz633";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xcosmocat/distributed-task-manager";
      versions = [
        {
          version = "v0.0.0-20240319203616-d19adc0dd4ab";
          hash = "0mpdy037x5amiznpwyfxvw3fb3xssclnw4clcw8vqqkjj06cwgdr";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xcosmocat/polkadotorchestrator";
      versions = [
        {
          version = "v0.0.0-20240319203321-7d9c8befa9e6";
          hash = "0nbc4kv3r9xgrh591bryxz3m30ykchin88nrq4dzd9hw2jzmmhd6";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xdaffifnty/tea-xyz-quest";
      versions = [
        {
          version = "v0.0.0-20240313152138-56afd194c040";
          hash = "14gbxlpif1k5ng5myzsf8lgxnyr96lnsc6fw15g6i0bjmnv0vj7x";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xdaniiel/keepawake";
      versions = [
        {
          version = "v0.0.0-20260304104746-366e06b38539";
          hash = "05q7n0kdr10nf7c7gpnd0935p23yfy84f14qdnd14c14yl03xqck";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xdq567/tea-addition";
      versions = [
        {
          version = "v0.0.0-20240229055034-fe4f8d68eaa3";
          hash = "0ryyblnkp88c59ny7n60kw22zq041pnayrlk65dcbziplrhfvgpy";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xdq567/tea-geometry-calculator";
      versions = [
        {
          version = "v0.0.0-20240229055659-37bee16f9fed";
          hash = "0wh5s4pbzyypmdgspf6xixhxijzndmshwkigac17zznx5ms91mxl";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xfalh/bandarsabu";
      versions = [
        {
          version = "v0.0.0-20240312171239-902d3bab8d53";
          hash = "01pgpkrw0zbzxvfrvc18q75axqsphb9p2v2sc6xghbrzj53hbsv8";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xfaqih/mengeteh";
      versions = [
        {
          version = "v0.0.0-20240316100950-2b513756628a";
          hash = "1n1gv7djvhn2wn4qbizq4kd6q7i1q893cf7q6ijfa44skkgzs492";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xfe10/dynamic-actions";
      versions = [
        {
          version = "v0.0.1-toolchain";
          hash = "1wc4s9y5fkz97iwp42kg86xz0nmgkfg2fcsz7l0f5y05hxwqgqf4";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xfede/typed-a";
      versions = [
        {
          version = "v0.0.0-20140715142213-6b3c50b01b25";
          hash = "0vivca60y9glx3na7js7k8fz7sajicmpi49kin62hz7kjjvl505s";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xffcc/sum-0xffcc";
      versions = [
        {
          version = "v0.0.0-20240306142406-14ad16683544";
          hash = "1p1ap64z9w08wfj2316kmn1n5qk3d0s6fwgyih3s92z2jb0q6b79";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xflotus/0xflotus";
      versions = [
        {
          version = "v0.0.0-20250428092011-242a00bac3f8";
          hash = "0ry2p9yqi80rdb9d2dlplskz2kr9xljgs5nmwczmi2c7gba9haq4";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xftm/ftm-check-balance";
      versions = [
        {
          version = "v0.0.0-20240316032701-1ac7be1f9c0a";
          hash = "03hhnsl2gwwwcxma0pnvy4faq626gsg5g086ax9xai53j6hlp348";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xftm/ftm-create-evm-wallet";
      versions = [
        {
          version = "v0.0.0-20240310140146-9b9933fef8b2";
          hash = "1g60vi2cg8jrq2yj7prwhflg76dhnyyysbinbhgb19ysv4llwf0g";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xgoju/dptc";
      versions = [
        {
          version = "v0.0.0-20240312080202-e8abb1a80b99";
          hash = "13gispmi5j35kl393ic3pmi4dc8chxl0606b24d4x4n4b1izrq33";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xjwlabs/discord-rpc";
      versions = [
        {
          version = "v0.0.0-20240403181433-f5d5645734f3";
          hash = "1kis4vnkh2qgyc2ji1rq7s9h9dgqvkx8xv2yfvsc1i2jl7ixnxbs";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xkev21/toggclass";
      versions = [
        {
          version = "v0.0.0-20240316161814-c6c462c21e26";
          hash = "0gm683qqp0524l80d64yk6iaynpb2c490jymb1cjvppkh478bxab";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xkirizuna/aqsal-trabas";
      versions = [
        {
          version = "v0.0.0-20240320065020-ae026c848ed9";
          hash = "1d8yiclbkf0vmjm984876ylvs4p29jrm78n595y9hll15hs8q8fy";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xlightshine/shineinaja";
      versions = [
        {
          version = "v0.0.0-20240320130317-9c9792c7565c";
          hash = "0ffaxh4p004g3d1qyr9wsmrimn8zb48n3lnmhj11bc0y1gaixqaz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xluoluoyu/fl-message";
      versions = [
        {
          version = "v1.0.3";
          hash = "1h1cnry3gvj1ypqxg1151xf7m03y17si3rz4k59cwwyd5qi1ici9";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xmnix0/crypto-strategy-backtester";
      versions = [
        {
          version = "v0.0.0-20240312131942-52926cf4be77";
          hash = "0n6ngpvpgjjc86x3xnwsyr3h3wi9mwqb2j0rray04jx9nl35f96n";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xmnix0/portfolio-optimizer-crypto";
      versions = [
        {
          version = "v0.0.0-20240312131934-308cbccbed1e";
          hash = "0alfvifaq17sjsjmqppjbswmslr95pi2y02fi2jndwaj41a4dbkm";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xrpheus/xprime-tizenbrew";
      versions = [
        {
          version = "v0.0.0-20260204054054-dde8538bc66e";
          hash = "0jn625z70zxzb1zk8305nfw9z8pw2ql164sj0c9ql1bdmp0c1y4b";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xsequence/kit";
      versions = [
        {
          version = "v0.0.0-20250305104427-3a0beac8e211";
          hash = "0n8xs5ghg1gjah9s9cj8qpijaybpq6hn5jmmfdzq4zfrhg0ly0pl";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xsuid/tiny";
      versions = [
        {
          version = "v0.0.0-20190102222629-bacda26d28a9";
          hash = "08adwfpp9m5lhvykycc4rd91q5b1wki8mksp4bsywh8i0lprfd83";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xsuid/winbattery";
      versions = [
        {
          version = "v0.0.0-20190103030743-9d54ee24f0c1";
          hash = "1lzhm1a03q31acah0hakbjjcb6dq1yv5lnzvdhmxjzs2fmrz647f";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xtechtonic/meowdove-util";
      versions = [
        {
          version = "v0.0.0-20240319091955-4937d7646499";
          hash = "1mddhamk4clmdgkcjnjngyrmpz02sr3rki10q8mdmkib0dvbgv44";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/0xtechtonic/nft-meadow-brokerage";
      versions = [
        {
          version = "v0.0.0-20240319091803-359bc91b63cb";
          hash = "0bw1j7j725w6i9z55r8bm6s4hsp2rz29anxh3x45j0h99xrndbb2";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/100xforever/glory-degenerator";
      versions = [
        {
          version = "v0.0.0-20240320075636-c54ea21c074a";
          hash = "02qx4835656mcgmx7095vx8pm3dpi53qqlbh8vk36klg57cg51cg";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/2-3-5-7/hexo-filename-title";
      versions = [
        {
          version = "v0.0.0-20230520102756-1a8550eebfaa";
          hash = "0msf8bh68iv0iqhscdqxmw7443csqbhyb98xrfiiqm0nladfvq09";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/2-3-5-7/hexo-mermaid-lastest";
      versions = [
        {
          version = "v0.0.0-20230520123329-41742a49cd14";
          hash = "0bbjrmi8lz465kapg8lh8qxmxvbkxlsb5qyds9hdpra1ly7vkkli";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/90mph/security-totp";
      versions = [
        {
          version = "v0.0.0-20150426050442-595ef74b6e31";
          hash = "01v510jsdmai33v4g411piby6xrnvwardmkw5iy04n0615bzld4c";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/96368a/hexo-order-abbrlink";
      versions = [
        {
          version = "v0.0.0-20220808172510-91d7c7126310";
          hash = "0v3hmvm66zxv03vnmhl0hjyx1q04d4x75yv28myc12qxrrsxn2bv";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/abbw/hexo-plugin-gitee";
      versions = [
        {
          version = "v0.0.0-20200409114843-c9ef1ca87073";
          hash = "1k6wp9bix5arbn2i1dsfjszx5209bzvjjykdb0y32lcg0prk5jl2";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ablipan/hexo-renderer-nunjucks";
      versions = [
        {
          version = "v0.0.0-20160406151131-c504f6cfce5b";
          hash = "1cvng3s0yh7819chqzcsky78l4dx1saqdaiyrp6lwwjii3pjr2sv";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/acwars/hexo_renderer_pug";
      versions = [
        {
          version = "v0.0.0-20210204124849-f993c0771835";
          hash = "19z9y9vmi3c67q9xjzmk9fkzaicxjj4s9bmb9jgz1g0qz51q59i6";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/acwars/hexo_renderer_pug_berry";
      versions = [
        {
          version = "v0.0.0-20210204124849-f993c0771835";
          hash = "0v1l80n77pprz8pj27r5h2r4kybxix0hagx9ncim13wwcdkpbqwz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/acwong00/hexo-addlink";
      versions = [
        {
          version = "v0.0.0-20170616080426-3a6b7289d3d8";
          hash = "0nnxwckzdmv7bi2mf63gbhlnhnj6vp5c26yhm0yrw8fzq4442i8v";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/aforaditya/simplebase";
      versions = [
        {
          version = "v0.0.0-20230617180706-3f1d4d94f244";
          hash = "154ac89dd59fllcy0cb2axf5ls4khnvpnpjx8n1nayqna9rci29z";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/akarzim/hexo-tag-fontawesome";
      versions = [
        {
          version = "v0.0.0-20260617144930-d0e5517a2223";
          hash = "1qqqfp7n561ji62mgia26immdhgzni35qnnfpnrnd0lxgrnsldvx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/alinnezha/charty0x";
      versions = [
        {
          version = "v0.0.0-20240318040312-2ce931f65d3b";
          hash = "0gcarxmp6338rbsi7029j2pzj93yr48467b93r7qfcapd0hyri4p";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/allimist/hexo-generator-robotstxt";
      versions = [
        {
          version = "v0.0.0-20170524100534-5c77b09a13f7";
          hash = "0595p563w7j7364qjcxmgqvcnng4igznyhhp3549a5hxim4ld7m4";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/andrewpeterprifer/hexo-helper-obfuscate";
      versions = [
        {
          version = "v0.0.0-20151130142038-0cd821dbb9f0";
          hash = "16nik2v79a9qgcx4nragc0qimx1aabdw4jdmb1m7pzzbfr6z8jxk";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/andrewsuzuki/hexo-deployer-webdav";
      versions = [
        {
          version = "v0.0.0-20151020171522-6644955dad72";
          hash = "0y98q131szvliyh1qbk3a5dmx5c0ss4mggchbd5yrd9jd80ah26x";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/annthespy/test-module";
      versions = [
        {
          version = "v1.0.2";
          hash = "1kk25a6bzqsyam09wadn818lijvrqbiw47hnm1mnsa499jx2i04j";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/anteriovieira/hexo-deployer-copy";
      versions = [
        {
          version = "v0.0.0-20170727172740-2842e0fe6165";
          hash = "1rr7qml7icc67b9f39yy80p5yfyzl3sd0crgaha7bsv7jydlargq";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/anthonyweidai/hexo-robotstxt-multisitemaps";
      versions = [
        {
          version = "v0.0.0-20230815012911-5e1477e6890a";
          hash = "097a578wdap00wchsnhr1rr4dgfyyahjql9xb54x9m9s7fg4mm9h";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/apazzolini/eslint-hackmud";
      versions = [
        {
          version = "v0.0.0-20161106031532-08571b233a52";
          hash = "0vfvypi3dq0id3f9jkws5hcyxvsynlywc7qg89mcdfx4chcairph";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ashisherc/hexo-auto-excerpt";
      versions = [
        {
          version = "v0.0.0-20171028143402-e559e0a2edaa";
          hash = "0sfn3fbbqzyw3nv210nia58ank21p5aqnpd4jnbg20siyf4mqi88";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/asmith-ivedix/npm-security";
      versions = [
        {
          version = "v0.0.0-20170810121743-24f22902f7a7";
          hash = "0dbysbjzcbdz5d9n7wsgvfxsxjn01fsijwyf7x3msfxs1mgah392";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ayoayco/blog-utils";
      versions = [
        {
          version = "v0.0.0-20230601074143-06dae17adfa6";
          hash = "17z8c9i0q3lcb2w8766fdls1mbdhr668bq921m5dk5c4xksjp54f";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/bammoo/hexo-qiniu-images";
      versions = [
        {
          version = "v0.0.0-20151109071448-5a1895d02b2f";
          hash = "0nl5v3phj7j7f1gyixglhhsgp0d9fv98ahhdfdds86nc2f78jx0n";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/battlemidget/hexo-renderer-markdownjs";
      versions = [
        {
          version = "v0.0.0-20131123200433-b3cc6b55918f";
          hash = "04g70wqa63p4q8jwhp364hb610s2aa8zrwbmimakficplbbbghxz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/bent0b0x/knex-runner";
      versions = [
        {
          version = "v0.0.0-20160413034823-f5d386a54b81";
          hash = "132dvf8v351fs7fjrdvl6rr822k48phlqrbkyj9zjqhyjr1ans37";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/bmats/hexo-renderer-browserify";
      versions = [
        {
          version = "v0.0.0-20151024063357-c8842611a570";
          hash = "028w0f22zsrcndwn67v4cm2i96mfdh13xi59ssv4h775ghl0n8il";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/bmwant/hexo-post-link";
      versions = [
        {
          version = "v1.0.0";
          hash = "0xc1l8i5gwrqkb476kwgcr5sdh6n7gsffjacfafn46hfyg9j1p8l";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/boybeak/hexo-auto-date";
      versions = [
        {
          version = "v0.0.0-20240925043336-e2ff7dd78010";
          hash = "0mpm7r1q2msn1n1nax4rkhy7qlp8fkl8n6gmn9grb0dkk3bnq4ng";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/boybeak/hexo-auto-photos";
      versions = [
        {
          version = "v0.0.0-20240922075150-9196874e08d9";
          hash = "0pgqsfmkzs8d68s3iakyc603596yqgrybr4mggfx0zvmvr7y3whk";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/brad-bowie/hackmap";
      versions = [
        {
          version = "v0.0.0-20150416191916-7f8bdb15f9aa";
          hash = "0zyjm0ilh3f6ambly38hm3c2f1mqza7clf0r6ljlvi9wznswqdyx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/celestezj/hexo-history-calendar";
      versions = [
        {
          version = "v0.0.0-20230916114449-50b35cce110b";
          hash = "0jj864y0wkz7fx0fxczhsn9hziywv1828yfj99p5ml9ir2arq6kv";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/chambersoft/blog-js";
      versions = [
        {
          version = "v0.0.0-20150508212754-272979be2cf4";
          hash = "0irw7x37g0864mfil8idsjq47q4bz7a0p8c7n3nx68snib6qv741";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/charleshenryhugo/zhu-palindrome";
      versions = [
        {
          version = "v0.0.0-20190218151506-9f479fdcdae8";
          hash = "1sdgmk03fcq2f5bgbdrq3f9zhlir7bdilghg435sj4yzaiy78vzk";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/chen-qingyu/hexo-console-zhihu";
      versions = [
        {
          version = "v0.0.0-20240625031257-f06593cb27b5";
          hash = "0kvnw0bajwyc156xvbpi9vyxczcz3n9iiva9qpqigama2c7a7zdx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/chen-qingyu/hexo-markdown-image";
      versions = [
        {
          version = "v0.0.0-20240709134721-884fc4868472";
          hash = "12xa19s9y5zqhjysvjyy7wfdbzr06d6gnsl3xcgl9dak60fibj5w";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/chiefy/hexo-generator-json";
      versions = [
        {
          version = "v0.0.0-20140415204359-02bb4044f664";
          hash = "0nq27fnfg4wn0bfjmf0r7nyxjzwlxdzx0v7lrkzqil5x84hnrl1x";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ciic-ui/ciic-ui.github.io";
      versions = [
        {
          version = "v0.0.0-20211017093015-82c8b83dc5b9";
          hash = "0xvsrjd1157drblfb13mihms778fylzzvl4bbl87ml4g94wg72vb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cksachdev/blogspot";
      versions = [
        {
          version = "v0.0.0-20151224212114-aee71aaabffd";
          hash = "1bphgsg7d0jfbx9slxprkfknl1w9ggyi7rcaq9j9b34i622yv021";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cntoby/hexo-deployer-ftput";
      versions = [
        {
          version = "v0.0.0-20190220021423-e16cc17848f6";
          hash = "1r49rbc9n43vhl2acar409s18pazs04izak3vl2schvybw0cm4dh";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cnzsb/hexo-asset";
      versions = [
        {
          version = "v0.0.0-20190918120100-012cda02159f";
          hash = "0czchi7mnrg8wqw2lx72idd8hk5yvlqnzdwb1nc1sachiark5xqc";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/codocs/codocs.github.io";
      versions = [
        {
          version = "v0.0.0-20170130135608-22d1d3cd7f74";
          hash = "143jb0rgkfi3q274nrbvs5wday10ahkqvqc60xc1vfgl4yisr88v";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/comarch-cybersecurity/tproecc_server";
      versions = [
        {
          version = "v0.0.0-20161104134750-f5433656b06a";
          hash = "0gihi2nwkf3asg86kn7hl853cq4p0833qcl5wcpzm9iy96zpl4yq";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cryptodeli/api-security-framework";
      versions = [
        {
          version = "v0.0.0-20240307183421-229b7ea18c47";
          hash = "0jmi2y298fq0wpqvj0gf1dkz5yydyjq8ra9ykb6l2yipczzrkjih";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cryptodeli/api-security-optimizer";
      versions = [
        {
          version = "v0.0.0-20240307202055-1e92c41aa040";
          hash = "0lrlavx8ffzbcm6066as1k6pkxmsa9787239a211l8lmwp3n1j07";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cupcakearmy/npm-security-walkthrough";
      versions = [
        {
          version = "v0.0.0-20210406162659-e58ffdae05e0";
          hash = "0686hc8afxz1a0b609iws6wy0z1k67v7swb7f0h8xhvkiw41bmkz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/cxmf234/cxm.github.io";
      versions = [
        {
          version = "v0.0.0-20200507064942-3a1383dfe52b";
          hash = "0lsnjp7mlb5063hg91mwv8mpc992bdv32vi3rmri7i9cww61nvqw";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dacarley/root-path";
      versions = [
        {
          version = "v0.0.0-20171020004221-1d0e411e8129";
          hash = "1ayxld37g9c8mx81d6vqnjh97cr930g0g1rb53bvfh50qv3sxp0q";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/davidtorcivia/theses";
      versions = [
        {
          version = "v0.0.0-20260917233051-74fbacfb9694";
          hash = "1qfzn2vrfplvz18lv9k1r17j8mr0l7rdynf7vas086pzsx8n4g3b";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/deadblue/hexo-heading-numbering";
      versions = [
        {
          version = "v1.0.1";
          hash = "03vclidybpfi3npa1j0xdhvh748mkn2syjnbb0fvrgvxscpsikpp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/deepak1556/hexo-renderer-myth";
      versions = [
        {
          version = "v0.0.0-20140424105250-37abce10bf67";
          hash = "1pfw1zd6vnxr6z0z23i7ahf2xnm5cvm4aldin17axz6yhsv7x0qp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/delpikye-v/hexon-react";
      versions = [
        {
          version = "v0.0.0-20251224101348-57a4332e0132";
          hash = "06p1sq4wgbq5lsp67mainr19a18mw6jf6997fvn6w087kdh99mga";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/devtox/hexo-nofollow-only";
      versions = [
        {
          version = "v0.0.0-20200721115906-a606bc566afe";
          hash = "073v8h2m55ld4sw8zcw7anq39vg1qgh4wmr6ajz04yszsj2xcxdw";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dirkehrenschwender/hackbay-beer";
      versions = [
        {
          version = "v0.0.0-20240515125813-1960aff4c08e";
          hash = "1bvaw0bi1dmdp66q0x3g9fsgcyhk4ypmax6cwjsziqgb5x6mla69";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dominictarr/hyperlightbox";
      versions = [
        {
          version = "v0.0.0-20160920132145-828fc08d6305";
          hash = "0yf62663xir20j9xh6cvzkxsnrk9qvh6nb8fmccqdpqrnq1nndsz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dominictarr/patchconfirm-lightbox";
      versions = [
        {
          version = "v0.0.0-20171209203312-41fd0f5c748f";
          hash = "1461c026zsazwp055y4d706wzkxlbixcmq1ljxpyfq6zd9wx3qpy";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dotintegral/hexo-tag-autogallery";
      versions = [
        {
          version = "v0.0.0-20180524103817-8d0ba90b6606";
          hash = "1k9c8883y1drbzm1s79b8drv2bnbmb7n56vm1979mp82569lsp8s";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dsreitan/dsreitan.github.io";
      versions = [
        {
          version = "v0.0.0-20250814124639-2ef2e6dcb466";
          hash = "09c6g2h6w3knmjfqq3hkvzrsn191flpxwqbcm9r7n3dj59xmdykd";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/dzwillia/hexo-helper-slugify";
      versions = [
        {
          version = "v0.0.0-20171204191804-72ce38420bac";
          hash = "1b9lhxhlrdcl5z53k6v8fpn0zql6vpq3vc2dqc0wx9nhpnx7ki8c";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/eai04191/hexo-tag-steam";
      versions = [
        {
          version = "v1.0.2";
          hash = "0sfigkscggbnwvq85m3flfik1vj1igcnyh74vrg6cb3qzk19qqsb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/elmariachi111/hexo-encore";
      versions = [
        {
          version = "v0.0.1";
          hash = "1n7bcnvcrkaicafy08y52b90kq2fyjx6gclvsb2ld5pj1wfcnsnn";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/elprup/hexo-migrator-pelican";
      versions = [
        {
          version = "v0.0.0-20151216074327-09bb76f3c7db";
          hash = "0893wabqb1b5phcz4adrhsgyp8y5ksq0hq5lczxf7yncd6pcqga8";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/emartech/koa-ui-security-middleware";
      versions = [
        {
          version = "v0.0.0-20181114104955-7f0407c75887";
          hash = "1h52r00bxd8nj5g2864n0f2xan8776vhlsz3s034f1v9aznl4pic";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/eminoda/hexo-jwt";
      versions = [
        {
          version = "v0.0.0-20180623143923-421f04dc1608";
          hash = "1snxlyhkn48wi6fkfcj10r7s37l8k2jjq49y9a78k9lljg0zi4md";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ericdouglas/rooted";
      versions = [
        {
          version = "v1.0.1";
          hash = "0yvk8araizby8j2kp98v8a68blq4bn7iczyc23qf18x7hl7hahlr";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ericelliott/rootrequire";
      versions = [
        {
          version = "v1.0.0";
          hash = "04x856hv11x8hvq7cyhz4y46h1ifj2nxpfs3xvpxg5y9h5n1pj8s";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/erkie/root-chain";
      versions = [
        {
          version = "v0.0.0-20140903053321-56c6807a2d8f";
          hash = "1k1xyny1zmkf7rhvhx5nzpjcm1zrysj3jdv7r2xmv3ilspkj3p0q";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/erkie/root-class.js";
      versions = [
        {
          version = "v0.0.0-20140501091530-1a60a9b0a609";
          hash = "0xblq73yz8rg98xxpapxdh2y1y7bg4wmibyp6sdyxmcrvyrnlvxv";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/eshugoel24/bitbucket-auth-require";
      versions = [
        {
          version = "v0.0.0-20180913105535-e5d3e7d2d744";
          hash = "1j9gbdgvay70xavg4hccs03cdg2hqy17zgim9cxvc59bmqzvgm5c";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/everblogjs/everblog-adaptor-hexo";
      versions = [
        {
          version = "v0.0.0-20171228095958-3c081de14bda";
          hash = "1wa0806xfy74lcfhilpdjdfg6h7j40dipfx7ryiz5fgnvp1kxafx";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/exploitenomah/mongoose-query-builder";
      versions = [
        {
          version = "v0.0.0-20231214130804-c58e3426652f";
          hash = "1sfmlclvd53kw42g29md73g6m50m2lfk86k5vygj9p7jcqaa9b5z";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/exploitmik/mike-package";
      versions = [
        {
          version = "v0.0.0-20210511162103-b71e60134647";
          hash = "0bhxr6c1wgr7685q97ig239098kh1h6i8rbwiibl8l7fr88p4xlz";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/expr/whack";
      versions = [
        {
          version = "v1.0.0";
          hash = "0rafq72s4cfx53f68ppjqmbnib0gc123zg4mvz0c28zykjj150pb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/f111fei/hexo-processor-copyassets";
      versions = [
        {
          version = "v0.0.0-20140817101121-3fe878af4724";
          hash = "0gidq1a3hvnzjzgnfgwlag9xgjy2mqv9apdks1625g5jhw27j6w9";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/f213/hexo-filter-typograf";
      versions = [
        {
          version = "v0.0.0-20160314090140-3353ae06480b";
          hash = "131l2968wx0lwryw06qfx1gs5swazll4r0a21m20di2rhm4knwdb";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fadehelix/hexo-tag-snack";
      versions = [
        {
          version = "v0.1.0";
          hash = "0yxwd4dkmhi60p5rfm9vjilrnl2f7x3fvqfr3whl886xkhlvgrrc";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fast-npm/fnpm";
      versions = [
        {
          version = "v0.0.0-20221128093518-82695f74bde4";
          hash = "0h39zifkzimfynd5b7zzdrflyx797fz91sggdw1pdqvbwxirhpsp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fdaciuk/hexo-static-search";
      versions = [
        {
          version = "v0.0.0-20150729010515-211cf15f0bcf";
          hash = "0crkqbvih8kcfvnix0vb2281bdv9cy5z0x5cmy8v5waafc953y43";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/feiju12138/hexo-next-mastodon-comments";
      versions = [
        {
          version = "v1.2.1";
          hash = "0b4wmjdkf7hnfpgfqcaf8dcivvzyvxygrbck5wam9w3qdssnqxsh";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/floriancargoet/hexo-deployer-ftp";
      versions = [
        {
          version = "v0.0.0-20150204102041-53421f2506f4";
          hash = "124xbx2688qqf582jjzk82rin7sqr3bcnsxxxbp2snanfq3d93jp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/followdev/notes-library";
      versions = [
        {
          version = "v0.0.0-20230711185445-a8948924eb6d";
          hash = "1x69jkrx03n37nhcqaqj3ggdmhgpgc7x0xwps65k8d7z66l1w06n";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/founda/security-holder";
      versions = [
        {
          version = "v0.0.0-20191116092448-a29d9e622f81";
          hash = "1ph7sbs5713mwlls4mk36260lhbjpsz1jbnbg5gwzpi6jfm34acs";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fr0stf0x/eslint-config-padding-spacing";
      versions = [
        {
          version = "v0.0.0-20220527085027-690969b45a7e";
          hash = "0xi68av9g0nva64gl0348fhrx8bxc5dpf3fx4ah0vc4r8w70g9i1";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fuchen/hexo-static-math";
      versions = [
        {
          version = "v0.1.0";
          hash = "1hw3vzsa1m6q69dmlba0yykl06i4kmvybfxdy374gwcbd3m2i12a";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fuchen/hexo-viz";
      versions = [
        {
          version = "v0.0.0-20170420125846-df0e330195b2";
          hash = "0s2dyavgspgwjcrgzwnk790wm13cls5iymigcqp6j7hbyppc1bad";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fusiongyro/ghost-to-jekyll";
      versions = [
        {
          version = "v0.0.0-20161130160803-892c2ad1b76c";
          hash = "0k0yll5mbvs09hj9k7c0p81vjkbnrqayaaavipcmb6g5m2q5ajdj";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fxspeiser/rustyweightbench";
      versions = [
        {
          version = "v0.0.0-20230403151037-9485aa17390b";
          hash = "0bn7q3vd41sgkyz3f1a57jxpybxs3z59nkr8avgslijpws7bdwn0";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fy00xx00/funnycode";
      versions = [
        {
          version = "v0.0.0-20160803102028-e2130b5ff184";
          hash = "1savilbif9qkgachn527qnfzlcbgj80bz5lphyj6h3pkmdga6k8p";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/fy1128/hexo-lang-switcher-btn";
      versions = [
        {
          version = "v0.0.0-20200104184718-3ecb86b64dce";
          hash = "1l2vpggl4pkkw57z1vxdvbr18crn6hc63698l0sgyw3s0wf6ap44";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/g100g/hexo-tag-eventbrite";
      versions = [
        {
          version = "v0.0.0-20161102163747-2a523bfc5fd7";
          hash = "0i3nyp88nqvsbfi6bj2mp8rfcxjaij0992kbspjd8fcjj26haira";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gaarf/term-hackernews";
      versions = [
        {
          version = "v0.2.0";
          hash = "18xd1b98bzbn3058w2m9sfly3cwvx6jnwlyr50fndjw2iyp8z1jy";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gabrfarina/hexo-jquery";
      versions = [
        {
          version = "v0.0.0-20160922044003-552919f77f14";
          hash = "10y7ajvnlb00yc2wccvkd1dyc2v70g3kgg6mdjd6h4236p7y049d";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ganlanshugod/vue-bana-springboot-plugin";
      versions = [
        {
          version = "v0.0.0-20171116034116-6d4ed9bf0c98";
          hash = "0w1anjr76ipv0zrdnylsmj5szkrb4qncrfy02jskqqb74jfy88av";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/geekrainy/hexo-tag-imgurl";
      versions = [
        {
          version = "v0.0.0-20220123024607-64e2138f2c4a";
          hash = "0663xy7chkw260cwx53isv16lyc6slv96zkhvw6bn11hjz3sdkmj";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/ghotst0xdeadbeef/js-utils";
      versions = [
        {
          version = "v0.0.0-20221109082616-fdbba2e91d97";
          hash = "0vmqfwgiv83awgqfrass3lwda7816qqbgmbw5a1sqb0wfc4j95jp";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gilmae/eightball";
      versions = [
        {
          version = "v0.0.0-20170916062520-770febb344a3";
          hash = "1b4v18i4146p9zib61bsbmd0di7yk1xgq85hcc4l89wssi4jfkw7";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/glyif/hexo-photo-camera-details";
      versions = [
        {
          version = "v0.0.0-20190224215438-473ba3c7c9c8";
          hash = "18m4q8q6p4mv5fj4wxj3pbfqiwq1hlxs421zjyk3dy8iyvdwxz18";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gunkdesign/hexo-invision";
      versions = [
        {
          version = "v0.0.0-20171120215936-b5c173a06b8c";
          hash = "1frh2657dgcirgnkx9l9qvnj896qa80c8nzzrs3rkbyly105r59i";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gunlor/gunlor.github.io";
      versions = [
        {
          version = "v0.0.0-20200319145611-e8c8af8de5e0";
          hash = "0q3bf170zrx696vks3cssyj40ix40vlgf3dr4359y5nypdxlxzc1";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/gyx138/hexo-tag-echarts4";
      versions = [
        {
          version = "v0.0.0-20191020104111-40a4fd723acc";
          hash = "1wx3rf22vaal6c3snf87nfz6a2jlz0vwqkchxslihp277i6fhh08";
        }
      ];
    }
    {
      ecosystem = "go";
      name = "github.com/h404bi/hexo-tag-ruby";
      versions = [
        {
          version = "v0.0.0-20171006094835-d54094b40aba";
          hash = "0gnxx2djr898vmw3d2gdndd5z42q3h6n2vj4z3jxgalqjqjpykbx";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.aayuslad:bikari-cp";
      versions = [
        {
          version = "1.0.0";
          hash = "1xrlglw1nf49l3svv1i9xfpipmh3zgn95xr681jiaqmhjngxy3sj";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.albanybuipe96:counter";
      versions = [
        {
          version = "1.0.0";
          hash = "18qsdm62c8yz0h7i8vv1fik00a5ijxssgg9xaak8wfy5zaiavpn6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.alice52:common-actuator-client";
      versions = [
        {
          version = "0.0.5";
          hash = "0fb3yz5dbp6g002p47dfyz2qg1j614ihyrnzcfscymrycd2n8a2w";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.andresguedes:AndroidSDK";
      versions = [
        {
          version = "1.0.0";
          hash = "1cka81jb75qf4rvg9ray8d5lpk2gpy4g1s0lg9j608nf99vd2s5i";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.andresviedma:trekkie-kotest";
      versions = [
        {
          version = "1.0.0-rc-1";
          hash = "06hdp3s2rd7wzqhzz5svshyrljn6zs1038c7ki5100frh8b4gh58";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.astinchoi:Akamai-AuthToken-Java";
      versions = [
        {
          version = "0.2.7";
          hash = "03zj3n9gqiyq44q1rwkahj3qcmgvljjhf0dbim72al0b2dh5xkrh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.baraawfares:easypdf-docx";
      versions = [
        {
          version = "0.7.0";
          hash = "0iv5h3kwhj7jwr7mgr3bw1pmmdgxdvffjwwzkvbpmbabhkfkh7im";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.billwrightcuprof:AlternateMazeObserver";
      versions = [
        {
          version = "4.5.0";
          hash = "1xyklg8wkz24f1f3i4hxx0iakzlncs9bjx5nhyazs5ivqaxp35x2";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.boswelja.accomplice:mobile-core";
      versions = [
        {
          version = "0.0.1";
          hash = "03332gs8fm4xj4wmyns6wfhykisnnsfkg9mw2bspiqd3bfznff1c";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.ceticamarco:LambdaTonic";
      versions = [
        {
          version = "0.0.4";
          hash = "14573zpa6h124zs3b6wag4rsvnw1rc2m68wih5c7mhwdrkj5z70y";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.chenkuan2000:watchers";
      versions = [
        {
          version = "1.0.0";
          hash = "1rfqxxsi9kjz4r18h31blwkapqs2rmjh238fbrc5kqcissz5w7wx";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.christoforosl:rest-results";
      versions = [
        {
          version = "0.7";
          hash = "0vkif9psff2xpgczf896xf9g342lq0vg4njhv2cgj8fx8xljlr8d";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.codegym6699:my-library";
      versions = [
        {
          version = "0.0.1";
          hash = "06c8944jv0fzw0q7arr69x1w8xhqfmb6hjirbjf801frk4spfc18";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.derangga:komig";
      versions = [
        {
          version = "0.1.0";
          hash = "05f0a90xlyyzjhmbb5nxjg296a9vdfppb4hr4wshif9c4r9yi4k9";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.dhlee1994:DHStringUtils";
      versions = [
        {
          version = "1.0.0";
          hash = "001ayhjfqr1f60c0j0l87qb18cscgz1vadps5nr1ny75ckcb2kaw";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.dmitrymysenko:textaroundcontent";
      versions = [
        {
          version = "0.1.0";
          hash = "0xa4bwx572y67gspr2zgybhbcq47lqxrd6kz4qsfr50m1qajy601";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.donaldfisher834:my-library";
      versions = [
        {
          version = "1.0.0";
          hash = "1wabki9jh8ixpx9rqx2kf5lr2b1rmvz5krb4qiblgs03rrgs5s07";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.dugtestorg:test11";
      versions = [
        {
          version = "1.0.0";
          hash = "06fbfsnr63kqxq145ic3dv92dypjrjqhxypqh0cicz9vd3js0yzr";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.emresagiroglu:common";
      versions = [
        {
          version = "1.1";
          hash = "12rgq5l8kz3vqh32lgxdlmx502lw7fyb8k2fh8hxkgjv52lajx6n";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.erwandemairy:test_maven";
      versions = [
        {
          version = "1.0.0";
          hash = "1yx6w9fwlzxiaz62khbh2k1pa9r8dhl0ilbj9bq3qrsk5ssidiws";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.faceplusplus:faceid-auth-oversea-publish-test-20260629133307";
      versions = [
        {
          version = "0.0.1";
          hash = "0ykrwili2vv0pwwvclflzq05xfm3cg3xyyq9x108xrjgl5jr2cnr";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.ginkgocity:matchwords";
      versions = [
        {
          version = "1.1";
          hash = "1s11z6qjmafnh1w5lplpachxns618lx67aa0magl3f1c18sf8ywg";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.girirajravichandran:internal-auth-utils";
      versions = [
        {
          version = "99.9.9";
          hash = "14kxhq9w35ii48jrvnmk4mi1p89pry6nl9ripdh04lpbjx9szccl";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.gironnetd:fibonacci";
      versions = [
        {
          version = "1.0.0";
          hash = "0iqsr7z92z9ihk4yv2nm6n6bmhsgdk8vr1xchhz0amsx6p52lqpi";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.gwyg:hello-maven";
      versions = [
        {
          version = "0.0.2";
          hash = "06xrbp1y3vrw409iym25063sph86ggqm5xck4cdqarjkh5bgpr8d";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.h4j4x.codegen:cli";
      versions = [
        {
          version = "0.0.6";
          hash = "148k565yzvr9v17jligay5h3s4z4cazwjdb3jxik9np85psb71ni";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.hc971225:hc-upload";
      versions = [
        {
          version = "0.0.1";
          hash = "12s1n4zxij9zzixwi3al0mxalfy35fz9gaczwk4jqm01jki16y5j";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.heyysudarshan:julie";
      versions = [
        {
          version = "1.0.0-alpha1";
          hash = "03mdkjx7yaivms3pjm03qsnrdqqb7bpn6dmwwwdn4dgx445dqh91";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.holtoon-bit:LeafAgent";
      versions = [
        {
          version = "1.0.0";
          hash = "1knf9sh04vmsxw071l8spnl35w9cnd2i7yfsxynphrccdf56nvf8";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.hyperchessbot:sonascalautils_2.12";
      versions = [
        {
          version = "1.0.0";
          hash = "1l3gk95wh1m8kakxgkp2aliqc9fjp2am9hyi7miz0ja65qclgasw";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.hypr2771:validated";
      versions = [
        {
          version = "1.3";
          hash = "1chpgzbgfl6gn88szrb9vd2jyc0z6mwwiizprbdcfwqpxxqbys97";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.iclonecode:smart-lang";
      versions = [
        {
          version = "0.1.2";
          hash = "0hs3pjl7c0zlfmdcq456nhgxppkk613bhbnzmr357ngcscfsfp1c";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.ilankumarani:ample";
      versions = [
        {
          version = "1.37.0";
          hash = "06q7awh7lxwn7p97qdp3wxfjazfgr7ab8jq9mbmyjvspm8a6yyq5";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.ing-vesper:vesper-codecs-xml_2.12";
      versions = [
        {
          version = "0.0.2";
          hash = "0yp58i9li1g1wsk736vdp1skxdsnlps79qfqvkl5j01g9ciwbw59";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.iroshperera:sinhala-transliterator";
      versions = [
        {
          version = "1.1.0";
          hash = "1hsy5pw0234vld1fdv5gfw2zadd0bx6vgvja5r8j2shny5f8mz0y";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.izpakla:limbo-ext";
      versions = [
        {
          version = "0.0.8";
          hash = "0vgfnq495nagjiafnc7ayb9chn38jdz4ghhr5nv2ygjsihbb1vlc";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.jja08111:markdown-toolbar-compose";
      versions = [
        {
          version = "0.1.0";
          hash = "0z7pnjw4qg7m6jyj1ap054q8hgfldmn26iq4fay22hp7kqcgblfm";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.jmsjtu:smoke-test-maven";
      versions = [
        {
          version = "0.0.0-19d5fef14f8-b4da00f5";
          hash = "1gp0j18hkd2222qzvq9x38mq3vz59870n483l0nw8s42a9ghssiy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.joeweh:http-utils";
      versions = [
        {
          version = "1.0";
          hash = "0ljvk85zrcapxgx7qblj69raq5zqyzyhafqz2ir08py3j4kjgnc0";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.jsanzo97:wickkit-compose";
      versions = [
        {
          version = "1.4.7";
          hash = "1h5572zljpca72y04c0z2i1j9jvg2080k5q2pkqjjh1cmm419xm7";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.jt-plug:joytalk";
      versions = [
        {
          version = "1.0.0-beta07";
          hash = "1vn9hs5p5ix0g97h3rbqsa6qjp28f3sgvp5iqajmpymq3czvjqwy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.kamenriderkuuga:oj-son";
      versions = [
        {
          version = "0.0.1";
          hash = "11g2hbbbmm61c9ncv1wvdgaxpr7pqznjbsnzjbm43i26zjb8w7dy";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.kleongf:Cubelib";
      versions = [
        {
          version = "1.0.0-beta.1";
          hash = "0md6dzxww2s6wa53i9cjfvnw27bj4bgvnlvs8jiv87s8c59i6yyp";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.kotlinbyte:scoped-state";
      versions = [
        {
          version = "1.0.0";
          hash = "1l21sjislsmqppn4r3rd88wgxlgp8kl2nwg4b3wzbwlbj8c8ki5x";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.kouleen:Interceptor";
      versions = [
        {
          version = "1.0.0";
          hash = "1df9v3c1v85p1f63rqgpbb0xcc4i18m947hi7bqmi47sj3l7r6rz";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lancelothuxi:mock-api";
      versions = [
        {
          version = "1.0.0";
          hash = "0l01lak8kwg2z4ylh4mgg1064n0jmk1ikvq7xjn26wag1rd3rh9p";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lathisskhumar:drishti-android";
      versions = [
        {
          version = "1.0.0";
          hash = "18qsdm62c8yz0h7i8vv1fik00a5ijxssgg9xaak8wfy5zaiavpn6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lavrov.src:cli_2.13";
      versions = [
        {
          version = "14";
          hash = "1r2qjard5jcb3bqgarl3wl8zldsf0s69g0bw083r77y2znf83w1q";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lbk-ones:dubbo3-spring-boot-starter";
      versions = [
        {
          version = "2.1.5";
          hash = "18myjfkckfv715dxig5jfp9adkrxi9aafzv3ycxybdss3k0lldw2";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.ldh8267937:vffmpeg";
      versions = [
        {
          version = "1.0.1";
          hash = "1pjvb5x1xg6qpcjanaa4ra20i64p39bxj56p3p2614d3b4g8dr4q";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lemcoder:acra-github";
      versions = [
        {
          version = "0.1.0";
          hash = "1j38fq5xn2zbpzj3q98zmfs1yhmp9d6dxfycqkflgzpivsn3w82r";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lenblazy:ScoreIndicator";
      versions = [
        {
          version = "1.0.3";
          hash = "0swbyg7nlr8gsa1wxmqfqzfprdjm3c0mk50rsabsqawvs7jxbnvi";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.leoregulus2002:utils-spring-boot-starter";
      versions = [
        {
          version = "1.0";
          hash = "1f7dmhmr8b7qkajqg3zw88f38ppx007nn38v0jg9s4529arshvws";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.letmutex.bubblepop:core";
      versions = [
        {
          version = "1.0.0";
          hash = "18qsdm62c8yz0h7i8vv1fik00a5ijxssgg9xaak8wfy5zaiavpn6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.lionpa:gradle-plugin";
      versions = [
        {
          version = "1.0.1";
          hash = "0mb0lvziqgkh0xrkdiw887fnjhhs7bzz7g4dc77mvgp6x7i6l812";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.logback-classmate:classmate";
      versions = [
        {
          version = "2.5.8";
          hash = "07jrbmfn2f46vxqps5rib70v625d607idsla6qhbrj21n31kx6im";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.maalwrldd:Myfirstpublishedpackage";
      versions = [
        {
          version = "1.0.0";
          hash = "093vjk01m0df83arlkpr19pccl4a3rwbahgkpyabgf0il3p7d6d5";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.mainmethod0126:annotation-scanner";
      versions = [
        {
          version = "0.0.4+jdk14";
          hash = "0xyanx9471by02vff4qln7irmfvhdg0jxc6v13vvdkqj15miiqj2";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.minxiangnan:common-utils";
      versions = [
        {
          version = "1.0.3";
          hash = "1yvn0gmhj4xq5np8js2ql4ab2x3c709vvs7y3c8in27qlpmzasaw";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.nadasleep:MyFirstPublishedPackage";
      versions = [
        {
          version = "1.0.0";
          hash = "1gxxjfb4l1nl7i2dc90fy1h6vfc3wv2hcpridcjb94c0xchvz51r";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.nguyenphuc22:TestMaven";
      versions = [
        {
          version = "1.0.0";
          hash = "1vd67amz05a05c8imwywzsy6zvj4iivr95rjisrn0ccy57bmz8i8";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.nicchongwb.ktjooqchecker:kotlin-ir-plugin";
      versions = [
        {
          version = "0.1.3";
          hash = "07i35mvh0dwmnzzd4h8207slrppkywnda3jm4pnxwvfgnqajxjm5";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.njain51.test:test-app";
      versions = [
        {
          version = "1.0.0";
          hash = "17cjym3lw4xn885yv3lm7jai9bi2iwz9dvajj96zhshnqcq7iawb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.nottamion:commandclip";
      versions = [
        {
          version = "1.1.0";
          hash = "0pda1mmrlchfh6a443ak5kvilk807sswqmap9yjjv90wrc20x35z";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.okumujustine:eazzyconnectsdk";
      versions = [
        {
          version = "0.1.1";
          hash = "0dyxwz5lqwr42fnk3rjz2dxlnaqybk3m3iqff0y5kfqvjpkww282";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.openunirest:object-mappers-gson";
      versions = [
        {
          version = "3.0.00";
          hash = "1mgnzr8mhfvmbr4sx5wmsn9zilgchqmbj76bv8fkay3vkfpn53kc";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.pengxurui:modular-eventbus-annotation";
      versions = [
        {
          version = "1.0.5";
          hash = "0b9p2a0rjvpf0gddlgd07yv2jrllw5wqx0r692h425gsas14m250";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.pepperkit:corenlp-stop-words-annotator";
      versions = [
        {
          version = "1.0.0";
          hash = "1wpdsm053nl7gjvrbmcns5fdrlnas9x0kpz7bf46qv661hbzdyrb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.piterrus0102:TestLibraryInMaven";
      versions = [
        {
          version = "1.0.1";
          hash = "05rydaia4jrgmx0mja8ngj7w8z34ix397c7kczybvqbp1hbq0xna";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.pixix4:KObserve";
      versions = [
        {
          version = "1.0.0-beta";
          hash = "18qsdm62c8yz0h7i8vv1fik00a5ijxssgg9xaak8wfy5zaiavpn6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.prosenjith:flowlens-core";
      versions = [
        {
          version = "0.1.0-alpha01";
          hash = "1bqqiz6k5yfai9afkakq5pkikj2aqf16n6qma3b0ja6705nhdsrk";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.remast:example";
      versions = [
        {
          version = "0.0.3";
          hash = "0w06rqqfbaiizwg1pa9vnmdln82c0xim8mv8v2dmqv4bx77nksvr";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.rewheeldev:common-utils";
      versions = [
        {
          version = "0.0.1";
          hash = "078s1yrmf319prnzcafarqd2x7qqqhridnpvip1rnj8dq52gws9k";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sam42r:semver-analyzer-api";
      versions = [
        {
          version = "1.9.1";
          hash = "0f30v2w6j7nqw76w8zf8780pchwnmi8wydwvaacg2li81f801ikb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.santhoshvernekar.wordcounter:word-counter";
      versions = [
        {
          version = "1.0.2";
          hash = "0xikdy8p1i4hixqy1wx0j4a7x2fb474y1jq53prrphvlw2mwr1qb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sheldonliu1227:mybatis-flex-spring-enhance";
      versions = [
        {
          version = "1.0.0";
          hash = "03lda4cb1j200qpr5gzjqflgc8zcbmzdfsfkp5f0gqi31qgw0irj";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.shivambhardwaj93:maven-publish-demo";
      versions = [
        {
          version = "1.0.0";
          hash = "18qan1x2z1g7jkvfsxmkxh5ds4avc5nb407pn6wnsl0l3rnbgfd8";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sitasp:raxati-core-manu";
      versions = [
        {
          version = "0.1.2";
          hash = "0jxzc7101dh6zdm90rddg38537lpjv63lnhgxdrfx5a4gkgy72q1";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.slyang520:dev-tool";
      versions = [
        {
          version = "1.3.8.RELEASE";
          hash = "1x6mywhx9ds783ymhqs6xnizjaa1bx3wjjrlslayrka258p52rpm";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.softmedtanzania:maskededittext";
      versions = [
        {
          version = "1.0.5";
          hash = "03pcq5h4dh90ncv09l2228r58yc6c7qyvmlg83mxvcl93x7s2l1z";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sohaib-ali-12:test-databricks-connect";
      versions = [
        {
          version = "1.0.2";
          hash = "107nvisyd8nzqr2qw7a2v4pk6xbyxyq0nz6cq0d50d9rzabyrwbn";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sparky17561:cube-root-generator";
      versions = [
        {
          version = "1.0.0";
          hash = "18vc96wq8gh3zhmrawqgh88b6rfhsv4fkmvnkbcmj9lzgz8vag17";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.stevdza-san:serpapi-kotlin";
      versions = [
        {
          version = "0.2.0";
          hash = "18qsdm62c8yz0h7i8vv1fik00a5ijxssgg9xaak8wfy5zaiavpn6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.studiorailgun:DataStructures";
      versions = [
        {
          version = "1.1.0";
          hash = "1fyrjw3m5z6fs7c0v66nz15d9s0bkjiqz9rv1r9flmzmza6sz73g";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.sung91:easyrbac-common";
      versions = [
        {
          version = "1.0.0";
          hash = "1lqcy9ml5gck23pdc9sk8y2w4x84kv0g4wg78043wd98h1jk07kh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.tcamise-gpsw:capitalize";
      versions = [
        {
          version = "1.0.0";
          hash = "0grdli0rcflrzwrxqjs9ylsnp20wq18gpqb9wzg68z43q7wv9dnh";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.theapache64:rebugger";
      versions = [
        {
          version = "1.0.1";
          hash = "0pshbam724a27lvm85wn71qd55w7rmynxb6wyj6rh3b4v46jjvif";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.thiyagu06:reactive-sqs-consumer";
      versions = [
        {
          version = "1.1";
          hash = "10apdkd8l97bg4mj0ih8y32ns0asn0n4a7r86s2aqa2gkhvfji7m";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.tisrop:json-resource-annotation";
      versions = [
        {
          version = "2.0.0";
          hash = "1y7f4g5s6vxjvcwfsna01wgjfvklz5hpdqn66wbfxl31rvnsh96m";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.tongbora:bakong-spring-boot-starter";
      versions = [
        {
          version = "1.0.8";
          hash = "19ya9a5klhvnbaigg9fjjmnylmsh0i4zbwjsy1fi1n0zzkz21pik";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.typebricks:pureconfig-toggleable_3";
      versions = [
        {
          version = "1.0.0";
          hash = "12w4ygzp21n959r22yy364ms5x5fvb5p5vr0v8xjjh0xf8hbpdrr";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.valerashimchuck:simpleitemgenerator-api";
      versions = [
        {
          version = "1.10.0";
          hash = "1mphf0z78fyx56vpm4wcl4nfjpc4syazjg0v6ccj7lcqskyv44ir";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.vincenzobaz:encoders_3";
      versions = [
        {
          version = "0.1.1";
          hash = "0fxpbv5p4awlcj6smq3ycamyfhwwmcmlf0g7wig5pqv9aj7frw94";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.volcengine:dataopen-sdk-java";
      versions = [
        {
          version = "1.0.8";
          hash = "054vngm371kb428399g979lcal3lfjw01p8i55vwycvrw6cn9d2r";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.yandroidua.kroute:koin-viewmodel";
      versions = [
        {
          version = "0.1.0";
          hash = "0y363m0jifmfnwmvmckvp4w62ppsg5j2ajw1hfjdpbvkqf3x1kq6";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.yannick-cw:contentful-cma-parser_2.12";
      versions = [
        {
          version = "0.0.4";
          hash = "0a7hb711gi3j4a2biap4800qf23k6hkw96rfrn3vkl7g1mk12acv";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.yasasbanukaofficial:mini-model-mapper";
      versions = [
        {
          version = "1.1.5";
          hash = "0r7mxlswlimrnlqzfri56dambakvhrrd41ykw5l31s4ivfyfb0fb";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.yskigpg:duration";
      versions = [
        {
          version = "0.1.0";
          hash = "02106b0ng2fywf495flrdw3p991l3z31r26sda67aphlxmsx4y69";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.yuexunshi:Nav";
      versions = [
        {
          version = "1.1.0";
          hash = "1n5w3mp1r5f584vm3l8dqyp01q7ans5m820ppz1ghisjafrg8l42";
        }
      ];
    }
    {
      ecosystem = "maven";
      name = "io.github.zhanleshun:openx-boot-booster";
      versions = [
        {
          version = "0.0.2";
          hash = "0fvfczdv09qw7wlhql1np2pz0yblsi1c7am0r1j275ckkm71dn8v";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Aore.Logger.SQLite";
      versions = [
        {
          version = "1.2.0";
          hash = "0mg27ilbgk98ghpdp3xqqb9x7y10vzac6qn4niipgg3z8k863pr1";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "APL.Net";
      versions = [
        {
          version = "1.0.0";
          hash = "0ym2pqh4f0plr27qy8hswrbpp0pp1vxjcsvh4ypw936cqqjdwhiy";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "AppCfg.Net";
      versions = [
        {
          version = "2.3.0";
          hash = "1fc1w26ln6gnwd5rif9qisqflw20pl61a81hyh7a2camrz505dmn";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Atelia.MdJson";
      versions = [
        {
          version = "0.1.0-preview.1";
          hash = "01swhwj3dw0y5aaiaa2ca9nfy5w6yrrsxwascbr9r4kypnk6amlh";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "AustinHarris.JsonRpc.AspNetCore";
      versions = [
        {
          version = "2.0.0-preview.1";
          hash = "1xbpppnbx4wp9lwqgpkh7jb5x47ri29nyna0m7agvldjazjp7c74";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "AustinHarris.JsonRpc.Newtonsoft";
      versions = [
        {
          version = "2.0.0-preview.1";
          hash = "119qcq92a2x92ks8fzhpirmcn7ci3knx60qhz9xnyqx61s0wkl52";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Beep.OM.Query";
      versions = [
        {
          version = "1.0.8-build.106";
          hash = "11ly739dy4k2hp2f78h7v7rvxgdqfpx0i4l11hq1wqvplsgz69v0";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Beep.OM.Tracker";
      versions = [
        {
          version = "1.0.8-build.106";
          hash = "0mpw6f25c5rd5nhiwp7h4jwd4zhmfqib0k2iml9qjr4b6lfzi96a";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Benevia.Core.Blobs";
      versions = [
        {
          version = "0.10.1-ci.472";
          hash = "1sk6fpwh9l1i2lr5nwicswswms8si4rrwcpdb4aclw1308fi9wkl";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Benevia.Core.Reports";
      versions = [
        {
          version = "0.10.1-ci.472";
          hash = "03n57dzw3dkgb3kl903bd2l8w9pkmybi92zh5ipijmpn9y05amiz";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "BHoM.Interop.LifeCycleAssessment";
      versions = [
        {
          version = "9.3.0-alpha.6.0";
          hash = "0arw2zq7d42gym2j2lm8ls634yp57582v4mv9zlcs06jxx3dvnby";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "BHoM.Interop.Mongo";
      versions = [
        {
          version = "9.3.0-alpha.6.0";
          hash = "0g54ygcksj7z3ah8vszcf39myy0fw7bmxcs7xqny2z1zgp5xcngg";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "BlazorNative.Core";
      versions = [
        {
          version = "0.16.3";
          hash = "15lh7ffqswifkagdil2l9plj94n0ykdhbk6iy42r3wwdqw17x2ka";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "BlazorNative.Testing";
      versions = [
        {
          version = "0.16.3";
          hash = "0laha736bgz88wid1wc4gq250jf04h5fs790mpjz25z5h4f7w32x";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "CloudL.EntityFrameworkCore";
      versions = [
        {
          version = "0.2.0";
          hash = "01njnbjcfwwxw77lfsxf2ghrrlc0cfjb1cl7j0r5mq71nw1ayn1y";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "CloudL.EntityFrameworkCore.PostgreSql";
      versions = [
        {
          version = "0.2.0";
          hash = "0181vmlx0gs3h0yq5204yqppvaj2g7ksljlrg7jwkyn2hpwbz79r";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "com.EBS.Common.Domain.Shared";
      versions = [
        {
          version = "1.6.7.48";
          hash = "18akb4b6g85107j45cxw4gb9p33grxq4lhbxzp0vfpkh2w5qii39";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Community.Pulumi.Osano";
      versions = [
        {
          version = "0.1.0";
          hash = "1ifrm33za4l7m2kp730cqzdjadjp6xwf8rfk7iv1x2p4mlgzbmmr";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Cratis.Stage.Rendering.Cratis.Scaffolding";
      versions = [
        {
          version = "4.16.1";
          hash = "11mm7789g7vd2mkxg9i4q5q4i7rflxc9i4qnf4hcr87vpj85qprp";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Cratis.Stage.Specifications";
      versions = [
        {
          version = "4.15.0";
          hash = "1nshb8399gq3rzj4gsnays2lg4caawglx5vjgc44ln2yc9nkihn5";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Credfeto.DotNet.Repo.Tools.Git.Interfaces";
      versions = [
        {
          version = "1.5.16.3794-main";
          hash = "1kvqdris267y1r5nhjlzdbagrw6rn34pl4fflx4xany176r09lah";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Credfeto.DotNet.Repo.Tools.Models";
      versions = [
        {
          version = "1.5.16.3794-main";
          hash = "1x5aidja7wjkjcxsn3h2zr7yanp6zsh5y4hbggggwv645z0bwikl";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "CW.Assistant.Extensions.Revit.2024";
      versions = [
        {
          version = "26.10.0-pullrequest834.113.66072";
          hash = "1fq367g03iziirzlnjfcsi1knqkzdzwnm9jr5dxixsvb1p2n3c9v";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "CW.Assistant.Extensions.Tekla.2023";
      versions = [
        {
          version = "26.10.0-pullrequest834.113.66072";
          hash = "1mhfr96lnm3bkbpdjclp2al0cry2f8bw2rq8dvnx4lj6cck03498";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Davish.Result.AspNetCore.Http";
      versions = [
        {
          version = "3.0.0";
          hash = "1gf7zbmdnnyrj4vkb6zlqf8h07jm8ii4xkzdrihrvq4sqhbxqnz5";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DebugBundle.AzureFunctions.Worker";
      versions = [
        {
          version = "2.0.0";
          hash = "0rl721r97k1iccp8bcsdgwbrq2357k31dh97ngi66gb9rlvb38cc";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DebugBundle.NLog";
      versions = [
        {
          version = "2.0.0";
          hash = "0fjrbca6vyackwl1ipkmycj57q89kx8wh2wx9v0x5hqnkd32nk4p";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DGKCwpfn10";
      versions = [
        {
          version = "7.5.5816";
          hash = "0b3apmpdyf780w5acc7a4ip59ng6a9sscwidw856z6n6cnp4qbcc";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DGKCwpfn8";
      versions = [
        {
          version = "7.5.5816";
          hash = "0hcndxc4z6p730jyp1c111g7rhj4rnvwpnmsyahv3g959ar4fdp0";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Dinaup.DTest";
      versions = [
        {
          version = "10.15.0.75";
          hash = "0ygqbaw99qgrg6jnyanlgdabgmbhipi7lxkmavc36a1n4dy4hv7l";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DotnetCliWrapper.Parsers.Build";
      versions = [
        {
          version = "0.2.2-dev.9";
          hash = "1bsk8lmllq8kw8jjk48wma9hba367vk637drz0yb4v2x6arm0h1j";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "DotnetCliWrapper.Parsers.Test";
      versions = [
        {
          version = "0.2.2-dev.9";
          hash = "1241dz4rdbw2z2izm9shsxs1ia4sj8r78q44dnccbvi9qfzdqybx";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EcoCore";
      versions = [
        {
          version = "7.2.0.17537";
          hash = "094i74bwy9z7jpmq2w2fc1d2ni9m3jjb14bwgrrwka4krr8fz0aa";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EcoWebASP";
      versions = [
        {
          version = "7.2.0.17537";
          hash = "0p2y2kpx448vyvb9ilnjw97gx49j48ia4dnh8h479a36p4vzfbwy";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EcoWebMVC";
      versions = [
        {
          version = "7.2.0.17537";
          hash = "01s63sv5fv521r7297h12a7ppqwi9kwdakzg7a1fss5ia9z7hddg";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EduardoQR.Civil3D.Benchmarking";
      versions = [
        {
          version = "2025.2.0";
          hash = "1hw9lss2phw8kjpmaw3m22hdlri7h24241004cq1b2dh91vigj46";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EduardoQR.Civil3D.Runtime";
      versions = [
        {
          version = "2027.2.0";
          hash = "1r5kbfqq7g6icylldwkrmxk5kyv115h536k1x3zp8vrnvnqyqz4a";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "ElmahCoreX.Common";
      versions = [
        {
          version = "2.2.13";
          hash = "0y067y1s746drfvvq17mbxhrfxfcfrrbmb2hp49gbg30waz0385d";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Empostor.Hazel";
      versions = [
        {
          version = "1.0.2";
          hash = "1jxqg8sd6z9ibi5h2zi2m7gcg6g40qljdnknfdp6mwx53yi6cja7";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EppLib";
      versions = [
        {
          version = "1.9.0";
          hash = "19z37dcc1ryi3nsir6xl8qw8xb53y0d0zm50jdm4x5nl0h20108r";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "EricksonLopez.Resilience.AspNetCore";
      versions = [
        {
          version = "2.0.0";
          hash = "1vq2a9jq5s2ahsv63nyycbwyd2jia9r5wg9qqmxglj6r8r2b4kc2";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Excalibur.Dispatch.Hosting.AspNetCore";
      versions = [
        {
          version = "10.0.0-alpha.12";
          hash = "15mqa5b9jgz078r5zy6pvmfcic5x90ji2b232j46n9j781xb6yd6";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Excalibur.Dispatch.Serialization.Avro";
      versions = [
        {
          version = "10.0.0-alpha.12";
          hash = "1s45in9z18p7h3w2jrn4577y7cqrm5k49vvyjgnnn66l89b0c49w";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "FunFair.BuildCheck.Interfaces";
      versions = [
        {
          version = "474.2.18.2741-github-actions";
          hash = "1cp9wzg9qdv2fqvzy4layjf576zy2fb22js0dl9v35qidw9m8qyj";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "GameRagKit";
      versions = [
        {
          version = "0.1.40";
          hash = "1ycq0ax3xs0rynqn78k2hiijyzrlvi33br83pc3q01bmgnk7xip0";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Gemstone.Web.Razor";
      versions = [
        {
          version = "1.0.183";
          hash = "1k6557wylf7yaib0l90rpbpnpzmnmvmvvgdqqbpcnmb5y1k4wkn7";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "ghul.analysis.protocol";
      versions = [
        {
          version = "62.9.1";
          hash = "19lkx24aflp31a1lbpx7igq5asiz5yv60yalh04wmhy4xsrcpab2";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Giraffe.Htmx";
      versions = [
        {
          version = "2.0.11";
          hash = "17phhvrql6jxbgvf237i3i25mm2r8qw91rmxpdimm87l7ywrhiil";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Giraffe.Htmx.Common";
      versions = [
        {
          version = "2.0.11";
          hash = "02spgc6aca4c1dpdvk52vqg5lcjvffrgwgfy1ghijhk97hsggyqd";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "GoogleCloudSpeech";
      versions = [
        {
          version = "0.0.0-dev.7";
          hash = "0cqjr5xzikqpqkp9a9m776cv0kn22wnasd3ybqk94pbkh808vmdf";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Innovabit.DotNet.Interceptors.Notifications.Email";
      versions = [
        {
          version = "1.0.0.99";
          hash = "1p8bdi06p1ihq9bck73cy1gv28c5wpsm31rb047ghn0fs3l58rnc";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Innovabit.DotNet.Interceptors.Transactions.AdoNet";
      versions = [
        {
          version = "1.0.0.99";
          hash = "15a0j98vhycj8405jpv4fn5mf8d9zdf1dw3azfgc9crgw342nr2j";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "JAG.JAGCL";
      versions = [
        {
          version = "10.0.3";
          hash = "0irxvlmgjw270qkq4r8vfvia5iywa53jpl5xb0nv075jp1il23gs";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "JsonPit";
      versions = [
        {
          version = "4.4.1";
          hash = "1q2aqahlqcv0p8b1k6fvx6d39k7vggm6xj54hgmb1y43dcarkx87";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Laraue.Apps.Identity.Internal.Contracts";
      versions = [
        {
          version = "0.0.3";
          hash = "0c25i0xrx0ib1svfqwa0pc0w3rz3w7vpckfqsc85sidzmby0zjqv";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Laraue.Telegram.NET.DataAccess";
      versions = [
        {
          version = "5.0.0";
          hash = "15h0mgpxpxyja1y3p7lsqxfmfhlbj3hmsdgadbva7b4584ga05mk";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "LibVLC4Sharp.WPF";
      versions = [
        {
          version = "4.0.0-nightly.202609250309";
          hash = "0q3046q23mg5fba0hhvb7vid2ymxzwxb0bcysmy1xmgaab0qzrf4";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "LogThis.NET.AspNetCore";
      versions = [
        {
          version = "0.1.0-beta.1";
          hash = "1pzh1fc8b0biadj97y38gip2742bya42c5rmx2fhnyl8c8qjdl0a";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Lucitex.Compression";
      versions = [
        {
          version = "0.0.5";
          hash = "1a2vylzixv5yyz36r1rcw90fjp5napayj06a7wwi4ix0bdwvpg41";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Lucitex.Jpeg";
      versions = [
        {
          version = "0.0.5";
          hash = "0ji7pkm2jwaayjmbryz5vn7myswbgns64r8cmsxarlxs1msxgsff";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MarkupString.Html";
      versions = [
        {
          version = "2.5.0";
          hash = "0i82b6gb1w6lkbzxysfcvqka6z20w514yncvha56anshb211fzzs";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MarkupString.Mxp";
      versions = [
        {
          version = "2.5.0";
          hash = "1lgqyh3da9ax6vvz453l9gynasld2drramsll5x5a49a7hzva4cm";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MDriven.Persistence.VistaDB.netstandard";
      versions = [
        {
          version = "7.2.0.17537";
          hash = "1vwi8az0xdkj5jqrv54rh7972mlvk44llxm20p40k9wxgzdjidch";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Meshmakers.Octo.Sdk.Adapters";
      versions = [
        {
          version = "3.4.132";
          hash = "03yby5aa8g2h6fcm4xbpajbzx5mdbhcp2hxkldpq52knm9wmr24x";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Meshmakers.Octo.Sdk.Packages.Environment";
      versions = [
        {
          version = "3.4.132";
          hash = "0mdzqh3q844rfjkv2nb2m7r675x37hr3wm42qnlrmkirgvx9q9d9";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Microlens.Cache";
      versions = [
        {
          version = "2.0.2";
          hash = "06l3hsfvj0xadpy9nav5b70iqrbs5g87fdsdafhbl2v9hx0mh4ps";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MintPlayer.AspNetCore.Endpoints.Abstractions";
      versions = [
        {
          version = "11.1.0-rc.0";
          hash = "01d6jaff4f1qxad57pjp1h1axc4zkqnl9jy1fn8ij95k8sp2xkkz";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Modgud.AspNetCore.ResourceServer";
      versions = [
        {
          version = "0.14.0";
          hash = "1k4br3q3215wvs1nj4k9va69gn4isrmdpdbm0hv5dplgycc59i0j";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "MSLX.SDK";
      versions = [
        {
          version = "1.7.0.1";
          hash = "1njx8frw2fkrxk2ip283hkmy9ryg7wyhsy8620wxzzd5gm2ci3vc";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Nodsoft.WowsReplaysUnpack.Benchmark";
      versions = [
        {
          version = "3.0.44-beta-ge52c560893";
          hash = "1qpxwkgw6j34ywfr14g33s80d00jps6kqcfizxnfqc5ddkmck6i0";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "NumberToWords.Core";
      versions = [
        {
          version = "1.0.3";
          hash = "18id0dkyim893bry4ml6d6hl7w4xgfx7nkz6rinqm6cxikyk1b95";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Oppex.Integration.Sdk";
      versions = [
        {
          version = "1.0.0";
          hash = "1wjxacrlv16d4kcp2971nh8rfgsknb26jbmlw2m4v8zzx5xid31d";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "PasteApeart.Application";
      versions = [
        {
          version = "26.9.22";
          hash = "0fjmcxblw9bhr8jlddx2x1naxvya9rj7b22y8r86793pzy82lw21";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "PasteApeart.Handler";
      versions = [
        {
          version = "26.9.22";
          hash = "1lmdl8b4v3gj40rf8b9bhqx3m46nc1ml6zlqjp259kn49scyysc2";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "PepperDash.Essentials.MobileControl";
      versions = [
        {
          version = "3.0.0-rc.4";
          hash = "0cdhfgcpyi1p590jhk96y5vp2z7m4klyx7q7s7r331bzkgr1725k";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Popolo.IO";
      versions = [
        {
          version = "4.0.0";
          hash = "1nfsa6hvs828w50g66mxqjnf32wld5l9lhns60rsc3xpzrip6d7f";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "PureFlac";
      versions = [
        {
          version = "0.1.0";
          hash = "0ay6m9mjljicm59gmi6is459xsaf6jkisn8bjdxfw62pwrdx7dzi";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "RaiDiagram";
      versions = [
        {
          version = "4.4.1";
          hash = "086xk47znskki2hzssqlvgdipcd7ifzwfpi0qby28wr5k7q5qq6s";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Rask.Testing";
      versions = [
        {
          version = "0.23.1-alpha.0.70";
          hash = "0nsd0dgkci1hd2lvnvxa0lpa88lsgqi8kgjp2b9vq821zygabg6i";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Rask.Validation.FluentValidation";
      versions = [
        {
          version = "0.23.1-alpha.0.68";
          hash = "0i0yjy0rxpr17lpy9a7v8zdgv7vl5jn37cha1k3gvd7sn27wqfhd";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Regula.FaceSDK.WebClient";
      versions = [
        {
          version = "8.4.831-rc";
          hash = "0zr1s1dralpm9pq6yqa8l29b3nlxnqq0rcmcymw2sp9a6cvm2irk";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Shiny.Blazor.Controls.Diagram";
      versions = [
        {
          version = "1.5.0-beta-0005";
          hash = "190cwjc0n4vdc24x1p1birw5zgk0hlq9xq4zgldk48nkykbrjj15";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Shiny.Maui.Controls.Camera.Ai";
      versions = [
        {
          version = "1.5.0-beta-0005";
          hash = "19vh8pckxrgyw48aarmmkjwwapp6ndr2grybl5kis0kq69g4qbb8";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "SIL.Chorus.ChorusMerge";
      versions = [
        {
          version = "6.0.0-beta0077";
          hash = "1x21pim200nh88a39c04d9gkimbd0byd9fsy0wmp65i8kibmyda0";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "SIL.Chorus.LibChorus.TestUtilities";
      versions = [
        {
          version = "6.0.0-beta0077";
          hash = "0jwpjqkiksh6923l3c4wlv6zs3mg2wvbjjv3bxc4cmr47b008b8r";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Soenneker.Stripe.Enums.SetupIntentUsage";
      versions = [
        {
          version = "4.0.57";
          hash = "1f36765xpjzbjg5pjp9bafln4asl5119xgi5p3bh5431y14qphp5";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Soenneker.Swashbuckle.IntellenumSchemaFilter";
      versions = [
        {
          version = "4.0.375";
          hash = "1mxfrxmk24kal5irw0y60cfjz4qyzbclqf4vha90wc467gdv579f";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "SoftwareExtravaganza.Whizbang.Hosting.RabbitMQ";
      versions = [
        {
          version = "0.2451.0-alpha.71";
          hash = "1rvxsm67mji7df5jx6p3yb30h9qzwlp67r14w13kgy5pirwq245f";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "SoftwareExtravaganza.Whizbang.Transports.Mutations";
      versions = [
        {
          version = "0.2451.0-alpha.71";
          hash = "0rk8hzq17sr78h9666w4dgpf7khi4v64pkp1njk5ypjh8c8f5x0g";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Tagua.Excel";
      versions = [
        {
          version = "0.2.0";
          hash = "02l404ywbv4rghf7s22nl1n6lflxldsdzh14w0hwyfxvs64awfvq";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Toggly.FeatureManagement.Blazor.Server";
      versions = [
        {
          version = "3.11.0";
          hash = "08gq98c03662621y0q3zvdy1gp4j40fr73rvqzhncciy5n3vjx93";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "TurtlePath.ExceptionHandling.Workers";
      versions = [
        {
          version = "1.6.5";
          hash = "064rw5gxkc94bmy9gapxpds49k2489iypdcrc8lcd0bl5xsd3fv1";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "TurtlePath.OctoMap";
      versions = [
        {
          version = "1.6.5";
          hash = "145y3w67rk5pxi5b7123yhzby0pdrkwb6ipzcrm076h4w78jn8x8";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Uno.UI.Adapter.Microsoft.Extensions.Logging";
      versions = [
        {
          version = "7.0.0-dev.1192";
          hash = "16hzgx7gs6s4iak59pz213lkjf8h2cc4sj3iw9kbnp4r0js0vwkd";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Webority.Azure.DataProtection";
      versions = [
        {
          version = "0.5.1";
          hash = "1hs50fm035fnq387c1s31jd9yr8q7id23yyif6fgs747adgwbi98";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Webority.Azure.NotificationHubs";
      versions = [
        {
          version = "0.5.1";
          hash = "0i87h84r21prnqg22w34jl0jwy7lqh5d06cn57igx4va56w5vwp6";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "ZeroAlloc.Cache.Generator";
      versions = [
        {
          version = "1.1.47";
          hash = "15j0qq7fnb43a05v1mrkpw4ziwn56qydw93ndp8jnkmp6qlawyvc";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "ZeroAlloc.Scheduling.Mediator";
      versions = [
        {
          version = "1.4.8";
          hash = "1fdhvcnkgzyf6awzagkzskkvyl2w9nl9gca7v951spxf2xy3kgx3";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Zespy.Gnaf.Client";
      versions = [
        {
          version = "1.6.0";
          hash = "1mcwxyz4kqdmnvjn1vaqc813n3fcd255dkfw9avyjhcjgngmkyx9";
        }
      ];
    }
    {
      ecosystem = "nuget";
      name = "Zespy.TruScan.Client.Extensions";
      versions = [
        {
          version = "1.7.0";
          hash = "139qm1jabmxnicqavbixzgrwnf8zkfnx6qfnpz6l8w7jmklvg342";
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
  # Compiled Java dependencies required only for three exceptional source
  # artifacts. `flake.nix` places these in `result/.class-path/`, never
  # extracts them, and `JavaProducer` receives them only through an explicit
  # per-entry `--class-path` setting. They can therefore resolve types without
  # becoming lowering input.
  java_classpath = [
    {
      name = "org.projectlombok:lombok";
      versions = [
        {
          version = "1.18.30";
          hash = "sha256-FBUbR1gtVwtN4WoUfs4729GazkruW946VXjIfbnsuZg=";
        }
      ];
    }
    {
      name = "org.projectlombok:lombok.patcher";
      versions = [
        {
          version = "0.48";
          url = "https://projectlombok.org/downloads/lombok.patcher-0.48.jar";
          hash = "1r1fxas7qjqd72av3cgsd9vpy09sx83d35vpqrqppd6xw3296jbv";
        }
      ];
    }
    {
      name = "zwitserloot.com:cmdreader";
      versions = [
        {
          version = "1.2";
          url = "https://projectlombok.org/ivyrepo/tools/com.zwitserloot.cmdreader-1.2.jar";
          hash = "144ikxhrhiz0ig3jlhclv261q9ms3bwwi23zbaca4afp68p6vv9g";
        }
      ];
    }
    {
      name = "org.apache.ant:ant";
      versions = [
        {
          version = "1.10.5";
          hash = "02sjqklkf5xsyl3dp43xxy2mi72lka6cbmgh500fsa89s3rp87x3";
        }
      ];
    }
    {
      name = "org.eclipse.jdt:ecj";
      versions = [
        {
          version = "3.32.0";
          hash = "1qk0cxmd33rddw2gvh43c27m3ra4qfsyfvm0jiihr7019b239q07";
        }
      ];
    }
    {
      name = "org.ow2.asm:asm";
      versions = [
        {
          version = "9.5";
          hash = "0lq31x32ls1m2di5ildq5da2gxv62g7k9iaq0hdpaa87k2sq8bmn";
        }
      ];
    }
    {
      name = "org.ow2.asm:asm-commons";
      versions = [
        {
          version = "9.5";
          hash = "1bn9254ds5xypy3wkm1a2pjvgvlclj2da3gjcfa8vpmrmzxykvkj";
        }
      ];
    }
    {
      name = "org.ow2.asm:asm-tree";
      versions = [
        {
          version = "9.5";
          hash = "156dkbflsm85aangp1py82g2z5akn54rmhdpmspawy8h354accrw";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.core.runtime";
      versions = [
        {
          version = "3.13.0";
          hash = "0a2sa0ll5xasm0zdwshc1dw61kdj336hi2mirffn1n5vn1p3qy2j";
        }
      ];
    }
    {
      name = "org.eclipse.jdt:org.eclipse.jdt.core";
      versions = [
        {
          version = "3.13.102";
          hash = "14ifa49azv7b5k8j96havhk00xgvn7vm8c837bcd9dz6d59jq08i";
        }
      ];
    }
    {
      name = "org.eclipse.jdt:org.eclipse.jdt.ui";
      versions = [
        {
          version = "3.13.100";
          hash = "1kb2h0280f3xgwpw59hzhphx585cfs25rp7h55xhj4h0m5gpmy1s";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.equinox.common";
      versions = [
        {
          version = "3.9.0";
          hash = "0qczsqaizn9cbyc171c79ql3w834g4ahhs1l34392ab98wpbn56y";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.equinox.registry";
      versions = [
        {
          version = "3.7.0";
          hash = "0gdzy9v1ldryr891iqw6ljfh0gz0875x0y5i3cwxwsp4vsj63jzn";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.equinox.app";
      versions = [
        {
          version = "1.3.400";
          hash = "1mj8w3jrfxsi0937rpwzg5myp3gd03kxbisxv2abkqhz07w0p1pc";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.core.resources";
      versions = [
        {
          version = "3.12.0";
          hash = "0nqyr4kallsbhzdq4yf68a8ia8nwid5sv0xddl6r6fkhshs98xjk";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.core.contenttype";
      versions = [
        {
          version = "3.6.0";
          hash = "1brgzvxi2hf7v6ksyn1sw0ly2rmirb3s291xyfjr8sir0vj1ckkd";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.core.jobs";
      versions = [
        {
          version = "3.9.0";
          hash = "00a2p56pb2bispg2qnvrkxg935ydrvix3jbazyisqpzybgmd5n2z";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.osgi";
      versions = [
        {
          version = "3.12.100";
          hash = "0njl185f8n2rsppx03i9spmcx0g724g47iqdkih845hqi28mxan1";
        }
      ];
    }
    {
      name = "org.eclipse.platform:org.eclipse.text";
      versions = [
        {
          version = "3.6.100";
          hash = "1nw7lxh9nfx6qkhnj6pc69cvqqpmak24xliwb0lsh4h50qb0lmz4";
        }
      ];
    }
    {
      name = "org.jetbrains.kotlin:kotlin-stdlib";
      versions = [
        {
          version = "1.3.72";
          hash = "sha256-OFanNJ66zW0b5oArL+2cTcLFpWTqkra5RayYgkPUsWs=";
        }
      ];
    }
    {
      name = "com.squareup.okhttp3:okhttp";
      versions = [
        {
          version = "3.14.9";
          hash = "sha256-JXD6tVUVy/iB16TO70n8UVSQvAJwV+Zmd2ooMkZa7KA=";
        }
      ];
    }
    {
      name = "com.squareup.okio:okio";
      versions = [
        {
          version = "1.17.2";
          hash = "sha256-+AzkLS/6xHrUxH4db5gNYE0kfOsaiGcFz0WBqwyf4rg=";
        }
      ];
    }
    {
      name = "org.codehaus.mojo:animal-sniffer-annotations";
      versions = [
        {
          version = "1.18";
          hash = "sha256-R/BYUrSO6brv74D6PYzqYO+kdTwAExId1/5e7y5ccp0=";
        }
      ];
    }
    {
      name = "com.google.android:android";
      versions = [
        {
          version = "4.1.1.4";
          hash = "sha256-hAclQcu3Ee/4n3J3EA/4VJKaRG26fOsbGVw0DgtP08s=";
        }
      ];
    }
    {
      name = "com.fasterxml.jackson.core:jackson-databind";
      versions = [
        {
          version = "2.16.1";
          hash = "sha256-uvio6+6PRe9ozdXi3Tkjs+KWwJN7luwLSAaqOjG8zR0=";
        }
      ];
    }
    {
      name = "com.fasterxml.jackson.core:jackson-core";
      versions = [
        {
          version = "2.16.1";
          hash = "sha256-9fjvkGCeZP7ILrkI5JfcfYGy65g/5Qm4cCkqGTzeTfs=";
        }
      ];
    }
    {
      name = "com.fasterxml.jackson.core:jackson-annotations";
      versions = [
        {
          version = "2.16.1";
          hash = "sha256-pHMHceakld03k6Qs24zmvduWx34V9AyY/Y2aeuCecoY=";
        }
      ];
    }
  ];
}
