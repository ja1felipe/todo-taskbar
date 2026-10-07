fn main() {
    // Em desenvolvimento as credenciais vêm do `.env` (na raiz do projeto); em
    // CI/release, de variáveis de ambiente passadas ao build. Embutimos o par
    // público (URL + anon key) para o app funcionar no mobile, onde não há
    // `.env` em runtime. A anon key pode ser embutida: quem limita o acesso são
    // as políticas de RLS.
    let _ = dotenvy::dotenv();

    println!("cargo:rerun-if-env-changed=SUPABASE_URL");
    println!("cargo:rerun-if-env-changed=SUPABASE_ANON_KEY");
    // O `dotenvy` procura o `.env` a partir do diretório do crate (`src-tauri`),
    // então ele vive um nível acima. Declaramos para o cargo reexecutar o build
    // script quando as credenciais mudarem.
    println!("cargo:rerun-if-changed=../.env");

    let url = std::env::var("SUPABASE_URL").unwrap_or_default();
    let anon = std::env::var("SUPABASE_ANON_KEY").unwrap_or_default();
    println!("cargo:rustc-env=SUPABASE_URL_BUILD={}", url);
    println!("cargo:rustc-env=SUPABASE_ANON_KEY_BUILD={}", anon);

    tauri_build::build()
}
